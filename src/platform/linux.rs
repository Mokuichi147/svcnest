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

/// 保存済みの unit の ExecStart を、quote の逆変換で引数へ分ける。
pub(crate) fn registered_args(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let Some(line) = text
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
    else {
        return Vec::new();
    };
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if_eq(&' ').is_some() {}
        let Some(first) = chars.next() else {
            break;
        };
        let quoted = first == '"';
        let mut arg = String::new();
        let mut next = if quoted { chars.next() } else { Some(first) };
        while let Some(character) = next {
            match character {
                '"' if quoted => break,
                ' ' if !quoted => break,
                '\\' => match chars.next() {
                    Some('n') => arg.push('\n'),
                    Some('r') => arg.push('\r'),
                    Some('t') => arg.push('\t'),
                    Some(other) => arg.push(other),
                    None => break,
                },
                '%' | '$' => {
                    chars.next_if_eq(&character);
                    arg.push(character);
                }
                other => arg.push(other),
            }
            next = chars.next();
        }
        args.push(arg);
    }
    args
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_args_reverse_quote() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(temp.path().join("state %h $HOME"))).unwrap();
        let runtime = temp.path().join("bin/run\"time\\x");
        let source = temp.path().join("install\tdir\nsvcnest");
        let definition = render_runtime(&paths, &runtime, &source);
        assert_eq!(
            registered_args(definition.as_bytes()),
            [
                runtime.to_string_lossy().into_owned(),
                "--home".into(),
                paths.home.to_string_lossy().into_owned(),
                "daemon".into(),
                "serve".into(),
                "--source-executable".into(),
                source.to_string_lossy().into_owned(),
            ]
        );
        // 1.0.0 より前の unit は起動役にインストール先を直接指定していた。
        assert_eq!(
            registered_args(b"[Service]\nExecStart=\"/home/me/.cargo/bin/svcnest\" --home \"/home/me/.config/svcnest\" daemon serve\n"),
            ["/home/me/.cargo/bin/svcnest", "--home", "/home/me/.config/svcnest", "daemon", "serve"]
        );
    }
}
