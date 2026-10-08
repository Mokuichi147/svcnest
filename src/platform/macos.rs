use crate::{
    paths::Paths,
    platform::{checked_command, label, xml_escape},
};
use anyhow::Result;
use std::path::Path;

fn domain() -> String {
    format!("gui/{}", unsafe { libc::geteuid() })
}

pub fn render(paths: &Paths, executable: &Path) -> String {
    let args = [
        executable.to_string_lossy().into_owned(),
        "--home".into(),
        paths.home.to_string_lossy().into_owned(),
        "daemon".into(),
        "serve".into(),
    ];
    let arguments = args
        .iter()
        .map(|arg| format!("    <string>{}</string>", xml_escape(arg)))
        .collect::<Vec<_>>()
        .join("\n");
    let log = xml_escape(&paths.logs.join("daemon.log").to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n  <key>Label</key><string>io.{}</string>\n  <key>ProgramArguments</key><array>\n{}\n  </array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n  <key>ThrottleInterval</key><integer>2</integer>\n  <key>ProcessType</key><string>Background</string>\n  <key>Umask</key><integer>63</integer>\n  <key>StandardOutPath</key><string>{log}</string>\n  <key>StandardErrorPath</key><string>{log}</string>\n</dict></plist>\n",
        label(paths),
        arguments
    )
}

pub async fn install(_paths: &Paths, path: &Path) -> Result<()> {
    checked_command(
        "launchctl",
        &[
            "bootstrap".into(),
            domain().into(),
            path.as_os_str().to_owned(),
        ],
    )
    .await?;
    Ok(())
}
pub async fn uninstall(paths: &Paths) -> Result<()> {
    checked_command(
        "launchctl",
        &[
            "bootout".into(),
            format!("{}/io.{}", domain(), label(paths)).into(),
        ],
    )
    .await?;
    Ok(())
}
pub async fn status(paths: &Paths) -> Result<String> {
    checked_command(
        "launchctl",
        &[
            "print".into(),
            format!("{}/io.{}", domain(), label(paths)).into(),
        ],
    )
    .await
}
