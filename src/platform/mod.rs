use crate::{
    error::fail,
    paths::{Paths, atomic_write, path_hash},
};
use anyhow::{Context, Result};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(windows)]
pub mod windows;

pub fn kind() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "launchd LaunchAgent"
    }
    #[cfg(target_os = "linux")]
    {
        "systemd --user"
    }
    #[cfg(windows)]
    {
        "Task Scheduler (current user, at logon)"
    }
}

pub fn label(paths: &Paths) -> String {
    format!("svcnest-{:016x}", path_hash(&paths.home))
}

pub fn registration_path(paths: &Paths) -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(user_home()?
            .join("Library/LaunchAgents")
            .join(format!("io.{}.plist", label(paths))))
    }
    #[cfg(target_os = "linux")]
    {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or(user_home()?.join(".config"));
        Ok(root
            .join("systemd/user")
            .join(format!("{}.service", label(paths))))
    }
    #[cfg(windows)]
    {
        Ok(paths.home.join("daemon-task.xml"))
    }
}

#[cfg(unix)]
fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn registered(paths: &Paths) -> Result<bool> {
    Ok(registration_path(paths)?.is_file())
}

pub fn render(paths: &Paths, executable: &Path) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        macos::render(paths, executable)
    }
    #[cfg(target_os = "linux")]
    {
        Ok(linux::render(paths, executable))
    }
    #[cfg(windows)]
    {
        windows::render(paths, executable)
    }
}

pub async fn install(paths: &Paths) -> Result<()> {
    let loaded = registered(paths)? && registration_loaded(paths).await;
    #[cfg(target_os = "macos")]
    let definition = {
        let source = fs::canonicalize(std::env::current_exe()?)?;
        if loaded
            && macos::registration_current(paths, &source, &fs::read(registration_path(paths)?)?)
        {
            return Ok(());
        }
        let runtime = crate::runtime::prepare(paths, &source)?;
        macos::render_runtime(paths, &runtime, &source)
    };
    #[cfg(windows)]
    let definition = {
        let source = fs::canonicalize(std::env::current_exe()?)?;
        let runtime = crate::runtime::prepare(paths, &source)?;
        windows::render_runtime(paths, &runtime, &source)?
    };
    if loaded {
        #[cfg(windows)]
        if fs::read(registration_path(paths)?)? == windows::registration_bytes(&definition) {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        macos::uninstall(paths).await?;
        #[cfg(not(any(windows, target_os = "macos")))]
        return Ok(());
    }
    let path = registration_path(paths)?;
    let previous = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    fs::create_dir_all(path.parent().context("Registration file has no parent")?)?;
    #[cfg(not(any(windows, target_os = "macos")))]
    let definition = render(paths, &fs::canonicalize(std::env::current_exe()?)?)?;
    #[cfg(windows)]
    let bytes = windows::registration_bytes(&definition);
    #[cfg(not(windows))]
    let bytes = definition.into_bytes();
    atomic_write(&path, &bytes)?;
    let result = {
        #[cfg(target_os = "macos")]
        {
            macos::install(paths, &path).await
        }
        #[cfg(target_os = "linux")]
        {
            linux::install(paths).await
        }
        #[cfg(windows)]
        {
            windows::install(paths, &path).await
        }
    };
    if let Err(error) = result {
        // 修復に失敗しても、以前のログイン時登録定義は失わない。
        if let Some(bytes) = previous {
            atomic_write(&path, &bytes)?;
            #[cfg(target_os = "macos")]
            if loaded {
                let _ = macos::install(paths, &path).await;
            }
        } else {
            crate::logging::remove_if_exists(&path)?;
        }
        return Err(error);
    }
    Ok(())
}

async fn registration_loaded(paths: &Paths) -> bool {
    let Ok(status) = integration_status(paths).await else {
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        status.lines().any(|line| line == "LoadState=loaded")
            && status.lines().any(|line| line == "UnitFileState=enabled")
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = status;
        true
    }
}

pub async fn uninstall(paths: &Paths) -> Result<()> {
    let path = registration_path(paths)?;
    #[cfg(target_os = "macos")]
    macos::uninstall(paths).await?;
    #[cfg(target_os = "linux")]
    linux::uninstall(paths).await?;
    #[cfg(windows)]
    windows::uninstall(paths).await?;
    crate::logging::remove_if_exists(&path)?;
    #[cfg(target_os = "linux")]
    checked_command("systemctl", &["--user".into(), "daemon-reload".into()]).await?;
    Ok(())
}

pub async fn integration_status(paths: &Paths) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        macos::status(paths).await
    }
    #[cfg(target_os = "linux")]
    {
        linux::status(paths).await
    }
    #[cfg(windows)]
    {
        windows::status(paths).await
    }
}

pub async fn checked_command(program: &str, args: &[std::ffi::OsString]) -> Result<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(program)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .context("OS integration command timed out")?
    .with_context(|| format!("Cannot execute {program}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return fail(
            "OS_INTEGRATION_FAILED",
            format!(
                "{program} failed: {}",
                detail.chars().take(2000).collect::<String>().trim()
            ),
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
