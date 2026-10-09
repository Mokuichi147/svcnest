use crate::{
    paths::Paths,
    platform::{checked_command, label},
};
use anyhow::Result;
use std::path::Path;

pub fn quote(text: &str) -> String {
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
            .replace('\t', "\\t")
    )
}

pub fn render(paths: &Paths, executable: &Path) -> Result<String> {
    let runtime = crate::runtime::executable_path(paths, executable)?;
    Ok(render_runtime(paths, &runtime, executable))
}

pub(crate) fn render_runtime(paths: &Paths, runtime: &Path, source: &Path) -> String {
    format!(
        "[Unit]\nDescription=svcnest per-user daemon\n\n[Service]\nType=simple\nExecStart={} --home {} daemon serve --source-executable {}\nRestart=on-failure\nRestartSec=2\nUMask=0077\nKillMode=mixed\nTimeoutStopSec=660\n\n[Install]\nWantedBy=default.target\n",
        quote(&runtime.to_string_lossy()),
        quote(&paths.home.to_string_lossy()),
        quote(&source.to_string_lossy())
    )
}

pub async fn install(paths: &Paths) -> Result<()> {
    checked_command("systemctl", &["--user".into(), "daemon-reload".into()]).await?;
    checked_command(
        "systemctl",
        &[
            "--user".into(),
            "enable".into(),
            format!("{}.service", label(paths)).into(),
        ],
    )
    .await?;
    Ok(())
}
pub async fn uninstall(paths: &Paths) -> Result<()> {
    checked_command(
        "systemctl",
        &[
            "--user".into(),
            "disable".into(),
            "--now".into(),
            format!("{}.service", label(paths)).into(),
        ],
    )
    .await?;
    Ok(())
}
pub async fn status(paths: &Paths) -> Result<String> {
    checked_command(
        "systemctl",
        &[
            "--user".into(),
            "show".into(),
            format!("{}.service", label(paths)).into(),
            "--property=LoadState,ActiveState,UnitFileState".into(),
        ],
    )
    .await
}
