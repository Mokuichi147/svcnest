use crate::{
    config::{ServiceConfig, validate_name},
    error::fail,
    paths::readable_path,
};
use anyhow::Result;
use std::{fs, path::Path};

pub fn resolve(
    configs: &[ServiceConfig],
    explicit: Option<&str>,
    cwd: &Path,
    all: bool,
    operation: &str,
) -> Result<Vec<ServiceConfig>> {
    if let Some(name) = explicit {
        validate_name(name)?;
        if all {
            return fail(
                "INVALID_TARGET",
                "A service name and --all cannot be used together",
            );
        }
        return configs
            .iter()
            .find(|config| config.name == name)
            .cloned()
            .map(|config| vec![config])
            .ok_or_else(|| {
                crate::error::ServiceError::new(
                    "SERVICE_NOT_FOUND",
                    format!("Service '{name}' is not registered"),
                )
                .into()
            });
    }
    let cwd = fs::canonicalize(cwd)?;
    for directory in cwd.ancestors() {
        let mut matches = configs
            .iter()
            .filter(|config| same_path(&config.cwd, directory))
            .cloned()
            .collect::<Vec<_>>();
        matches.sort_by(|a, b| a.name.cmp(&b.name));
        if matches.is_empty() {
            continue;
        }
        if matches.len() > 1 && !all {
            let names = matches
                .iter()
                .map(|config| format!("  {}", config.name))
                .collect::<Vec<_>>()
                .join("\n");
            let all_hint = if matches!(
                operation,
                "start" | "stop" | "restart" | "status" | "enable" | "disable" | "remove"
            ) {
                format!("\n\nor:\n\n  svcnest {operation} --all")
            } else {
                String::new()
            };
            return fail(
                "AMBIGUOUS_SERVICE",
                format!(
                    "Multiple services are registered for this directory:\n\n{names}\n\nSpecify a service:\n\n  svcnest {operation} {}{all_hint}",
                    matches[0].name
                ),
            );
        }
        return Ok(matches);
    }
    fail(
        "SERVICE_NOT_FOUND",
        format!(
            "No service is registered for {} or its parents",
            readable_path(&cwd).display()
        ),
    )
}

fn same_path(registered: &Path, current: &Path) -> bool {
    let canonical = fs::canonicalize(registered).unwrap_or_else(|_| registered.to_owned());
    #[cfg(windows)]
    {
        canonical
            .to_string_lossy()
            .eq_ignore_ascii_case(&current.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        canonical == current
    }
}
