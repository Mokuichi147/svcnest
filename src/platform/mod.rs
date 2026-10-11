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

/// 保存済みの登録定義が起動に使う実行ファイル。
/// 現行の定義は起動役のコピーと `--source-executable` の更新元、旧定義は起動役だけを返す。
pub fn registered_executables(paths: &Paths) -> Result<Vec<PathBuf>> {
    let bytes = match fs::read(registration_path(paths)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    Ok(executables_in(&bytes))
}

fn executables_in(bytes: &[u8]) -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let args = macos::registered_args(bytes);
    #[cfg(target_os = "linux")]
    let args = linux::registered_args(bytes);
    #[cfg(windows)]
    let args = windows::registered_args(bytes);
    let mut executables = Vec::new();
    executables.extend(args.first().map(PathBuf::from));
    if let Some(index) = args.iter().position(|arg| arg == "--source-executable") {
        executables.extend(args.get(index + 1).map(PathBuf::from));
    }
    executables
}

/// 登録定義が参照する実行ファイルのうち、存在しない最初のもの。
pub fn missing_executable(paths: &Paths) -> Result<Option<PathBuf>> {
    Ok(registered_executables(paths)?
        .into_iter()
        .find(|path| !path.is_file()))
}

/// 移動・削除された実行ファイルを参照する登録を、現在の実行ファイルで登録し直す。
/// 登録は enable / install で利用者が求めたものなので、壊れたままにせず次回のログインに備える。
pub async fn repair(paths: &Paths) -> Result<()> {
    let Some(missing) = missing_executable(paths)? else {
        return Ok(());
    };
    // launchd の再登録は bootout を伴い、登録から起動した daemon を止めるため、停止中だけ行う。
    #[cfg(target_os = "macos")]
    if crate::ipc::ping(paths).await {
        return Ok(());
    }
    install(paths).await.with_context(|| {
        format!(
            "Cannot repair the login registration that refers to missing {}",
            crate::paths::readable_path(&missing).display()
        )
    })
}

/// 現在の実行ファイルで登録済みなら、launchd に daemon を起動させる。起動を依頼したら true。
/// launchd が起動した daemon だけが KeepAlive で監視される。別の実行ファイルの登録は、
/// その版の daemon が起動するため使わない。
#[cfg(target_os = "macos")]
pub async fn start_registered(paths: &Paths) -> Result<bool> {
    let bytes = match fs::read(registration_path(paths)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let source = fs::canonicalize(std::env::current_exe()?)?;
    if !macos::registration_current(paths, &source, &bytes) || !registration_loaded(paths).await {
        return Ok(false);
    }
    macos::kickstart(paths).await?;
    Ok(true)
}

pub fn render(paths: &Paths, executable: &Path) -> Result<String> {
    #[cfg(target_os = "macos")]
    {
        macos::render(paths, executable)
    }
    #[cfg(target_os = "linux")]
    {
        linux::render(paths, executable)
    }
    #[cfg(windows)]
    {
        windows::render(paths, executable)
    }
}

pub async fn install(paths: &Paths) -> Result<()> {
    let loaded = registered(paths)? && registration_loaded(paths).await;
    #[cfg(any(unix, windows))]
    let source = fs::canonicalize(std::env::current_exe()?)?;
    #[cfg(any(unix, windows))]
    let runtime = crate::runtime::prepare(paths, &source)?;
    #[cfg(target_os = "macos")]
    let definition = macos::render_runtime(paths, &runtime, &source);
    #[cfg(target_os = "linux")]
    let definition = linux::render_runtime(paths, &runtime, &source);
    #[cfg(windows)]
    let definition = windows::render_runtime(paths, &runtime, &source)?;
    let path = registration_path(paths)?;
    let previous = match fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if loaded {
        #[cfg(target_os = "macos")]
        if macos::registration_current(paths, &source, previous.as_deref().unwrap_or_default()) {
            return Ok(());
        }
        #[cfg(target_os = "linux")]
        if previous.as_deref() == Some(definition.as_bytes()) {
            return Ok(());
        }
        #[cfg(windows)]
        if previous.as_deref() == Some(windows::registration_bytes(&definition).as_slice()) {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        macos::uninstall(paths).await?;
    }
    fs::create_dir_all(path.parent().context("Registration file has no parent")?)?;
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
            #[cfg(target_os = "linux")]
            let _ = checked_command("systemctl", &["--user".into(), "daemon-reload".into()]).await;
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

#[cfg(any(target_os = "macos", windows))]
pub fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    // macOS / Linux の登録先は実際の利用者の設定ディレクトリのため、定義はファイルに書かない。
    #[test]
    fn registered_executables_are_the_runtime_and_its_source() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(temp.path().join("state"))).unwrap();
        let source = temp.path().join("installed");
        let runtime = paths.home.join("bin").join("digest").join("svcnest");
        #[cfg(target_os = "macos")]
        let bytes = macos::render_runtime(&paths, &runtime, &source).into_bytes();
        #[cfg(target_os = "linux")]
        let bytes = linux::render_runtime(&paths, &runtime, &source).into_bytes();
        #[cfg(windows)]
        let bytes = windows::registration_bytes(
            &windows::render_runtime(&paths, &runtime, &source).unwrap(),
        );
        assert_eq!(executables_in(&bytes), [runtime, source]);
        assert!(executables_in(b"").is_empty());
    }
}
