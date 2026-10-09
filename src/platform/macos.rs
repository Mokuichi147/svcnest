use crate::{
    paths::Paths,
    platform::{checked_command, label, xml_escape},
};
use anyhow::Result;
use std::{fs, path::Path};

fn domain() -> String {
    format!("gui/{}", unsafe { libc::geteuid() })
}

pub fn render(paths: &Paths, executable: &Path) -> Result<String> {
    let runtime = crate::runtime::executable_path(paths, executable)?;
    Ok(render_runtime(paths, &runtime, executable))
}

pub(crate) fn render_runtime(paths: &Paths, runtime: &Path, source: &Path) -> String {
    let args = [
        runtime.to_string_lossy().into_owned(),
        "--home".into(),
        paths.home.to_string_lossy().into_owned(),
        "daemon".into(),
        "serve".into(),
        "--source-executable".into(),
        source.to_string_lossy().into_owned(),
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

pub(crate) fn registration_current(paths: &Paths, source: &Path, bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let Some((_, arguments)) = text.split_once("<key>ProgramArguments</key><array>\n    <string>")
    else {
        return false;
    };
    let Some((executable, _)) = arguments.split_once("</string>") else {
        return false;
    };
    let prefix = format!("{}/", xml_escape(&paths.home.join("bin").to_string_lossy()));
    let Some(digest) = executable
        .strip_prefix(&prefix)
        .and_then(|path| path.strip_suffix("/svcnest"))
    else {
        return false;
    };
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return false;
    }
    let runtime = paths.home.join("bin").join(digest).join("svcnest");
    // 登録済みの旧コピーを保持し、更新後の enable / install で bootout しない。
    text == render_runtime(paths, &runtime, source)
        && fs::symlink_metadata(&runtime)
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
        && crate::runtime::executable_path(paths, &runtime).is_ok_and(|path| path == runtime)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_keeps_its_bootstrap_across_updates_but_detects_changes() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(temp.path().join("state with & spaces"))).unwrap();
        let source = temp.path().join("installed with & spaces");
        fs::write(&source, b"first build").unwrap();
        let runtime = crate::runtime::prepare(&paths, &source).unwrap();
        let definition = render_runtime(&paths, &runtime, &source);
        assert!(registration_current(&paths, &source, definition.as_bytes()));
        fs::write(&source, b"second build").unwrap();
        assert!(registration_current(&paths, &source, definition.as_bytes()));
        assert!(!registration_current(
            &paths,
            &temp.path().join("another install"),
            definition.as_bytes()
        ));
        assert!(!registration_current(
            &paths,
            &source,
            definition.replace("<true/>", "<false/>").as_bytes()
        ));
        fs::write(runtime, b"damaged bootstrap").unwrap();
        assert!(!registration_current(
            &paths,
            &source,
            definition.as_bytes()
        ));
    }
}
