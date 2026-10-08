use crate::{
    error::fail,
    paths::{Paths, atomic_write},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    #[default]
    OnFailure,
    Always,
}

impl std::fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Never => "never",
            Self::OnFailure => "on-failure",
            Self::Always => "always",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub cwd: PathBuf,
    pub command: Vec<String>,
    pub resolved_executable: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_script: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interpreter_args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub interpreter_environment: BTreeMap<String, String>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_ms: u64,
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

pub fn default_stop_timeout() -> u64 {
    10_000
}

pub fn validate_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 64
        || !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit()
        || !bytes.iter().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
    {
        return fail(
            "INVALID_NAME",
            "Service names must match [a-z0-9][a-z0-9._-]{0,63}",
        );
    }
    Ok(())
}

pub fn validate_env_key(key: &str) -> Result<()> {
    if key.is_empty() || key.contains(['=', '\0']) {
        return fail(
            "INVALID_ENV",
            "Environment variable names cannot be empty or contain '=' or NUL",
        );
    }
    Ok(())
}

impl ServiceConfig {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        if self.version != 1 {
            return fail(
                "CONFIG_VERSION",
                format!("Unsupported config version: {}", self.version),
            );
        }
        if self.command.is_empty()
            || self.command[0].is_empty()
            || self.command.iter().any(|arg| arg.contains('\0'))
        {
            return fail(
                "INVALID_COMMAND",
                "Command must be a nonempty argv array without NUL",
            );
        }
        if !self.cwd.is_absolute()
            || !self.resolved_executable.is_absolute()
            || self
                .resolved_script
                .as_ref()
                .is_some_and(|p| !p.is_absolute())
            || self.env_file.as_ref().is_some_and(|p| !p.is_absolute())
        {
            return fail("INVALID_CONFIG", "Config paths must be absolute");
        }
        if self.interpreter_args.iter().any(|arg| arg.contains('\0'))
            || (!self.interpreter_args.is_empty() || !self.interpreter_environment.is_empty())
                && self.resolved_script.is_none()
        {
            return fail(
                "INVALID_CONFIG",
                "Interpreter settings require a resolved script; arguments cannot contain NUL",
            );
        }
        if self.stop_timeout_ms == 0 || self.stop_timeout_ms > 300_000 {
            return fail(
                "INVALID_CONFIG",
                "Stop timeout must be between 1 and 300000 milliseconds",
            );
        }
        for (key, value) in self.environment.iter().chain(&self.interpreter_environment) {
            validate_env_key(key)?;
            if value.contains('\0') {
                return fail("INVALID_ENV", "Environment values cannot contain NUL");
            }
        }
        Ok(())
    }

    pub fn effective_environment(&self) -> Result<BTreeMap<String, String>> {
        let mut env = self.interpreter_environment.clone();
        if let Some(path) = &self.env_file {
            let iter = dotenvy::from_path_iter(path)
                .map_err(|_| anyhow::anyhow!("Cannot read env-file {}", path.display()))?;
            for entry in iter {
                let (key, value) =
                    entry.map_err(|_| anyhow::anyhow!("Invalid env-file {}", path.display()))?;
                validate_env_key(&key)?;
                if value.contains('\0') {
                    return fail("INVALID_ENV", "Environment values cannot contain NUL");
                }
                insert_env(&mut env, key, value);
            }
        }
        for (key, value) in &self.environment {
            insert_env(&mut env, key.clone(), value.clone());
        }
        Ok(env)
    }

    pub fn redacted(&self) -> Self {
        let mut config = self.clone();
        for (key, value) in config
            .environment
            .iter_mut()
            .chain(&mut config.interpreter_environment)
        {
            if !key.eq_ignore_ascii_case("PATH") {
                *value = "********".to_owned();
            }
        }
        config
    }
}

pub fn insert_env(env: &mut BTreeMap<String, String>, key: String, value: String) {
    #[cfg(windows)]
    if let Some(old) = env
        .keys()
        .find(|old| old.eq_ignore_ascii_case(&key))
        .cloned()
    {
        env.remove(&old);
    }
    env.insert(key, value);
}

pub fn load(path: &Path) -> Result<ServiceConfig> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("Cannot read config {}", path.display()))?;
    // TOML の診断には入力値が含まれるため、秘密情報を CLI へ出さない。
    let config: ServiceConfig = toml::from_str(&text)
        .map_err(|_| anyhow::anyhow!("Invalid TOML config: {}", path.display()))?;
    config.validate()?;
    if path
        .file_stem()
        .and_then(|p| p.to_str())
        .and_then(|stem| stem.strip_prefix("svc-"))
        != Some(config.name.as_str())
    {
        return fail(
            "INVALID_CONFIG",
            "Service name does not match config filename",
        );
    }
    Ok(config)
}

pub fn load_named(paths: &Paths, name: &str) -> Result<ServiceConfig> {
    validate_name(name)?;
    if !paths.config(name).exists() {
        return fail(
            "SERVICE_NOT_FOUND",
            format!("Service '{name}' is not registered"),
        );
    }
    load(&paths.config(name))
}

pub fn all(paths: &Paths) -> Result<Vec<ServiceConfig>> {
    let mut configs = Vec::new();
    for path in config_files(paths)? {
        configs.push(load(&path)?);
    }
    configs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(configs)
}

pub fn config_files(paths: &Paths) -> Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(&paths.configs)?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    files.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "toml")
    });
    files.sort();
    Ok(files)
}

pub fn save(paths: &Paths, config: &ServiceConfig) -> Result<()> {
    config.validate()?;
    atomic_write(
        &paths.config(&config.name),
        toml::to_string_pretty(config)?.as_bytes(),
    )
}
