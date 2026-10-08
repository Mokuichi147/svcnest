use crate::{
    error::{ServiceError, fail},
    resolve::executable,
};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

// npm が生成する Node.js 用 .cmd の実体を解決する。一般の batch は None を返す。
pub async fn resolve(
    shim: &Path,
    cwd: &Path,
    path: Option<&OsStr>,
    environment: &BTreeMap<String, String>,
) -> Result<Option<(PathBuf, PathBuf)>> {
    let bytes = fs::read(shim).context("Cannot read batch launcher")?;
    if bytes.len() > 128 * 1024 {
        return Ok(None);
    }
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    let parent = shim.parent().context("Launcher has no parent directory")?;
    let (mut script, prefix) = match parse(text, parent) {
        Ok(parsed) => parsed,
        Err(error) if ServiceError::code(&error) == "SHELL_REQUIRED" => return Ok(None),
        Err(error) => return Err(error),
    };
    let adjacent_node = parent.join("node.exe");
    let node = if executable::is_executable(&adjacent_node) {
        fs::canonicalize(adjacent_node)?
    } else {
        executable::resolve("node", cwd, path, Some(OsStr::new(".EXE")))?
    };
    if let Some(prefix_script) = prefix {
        // npm 自身と同じ prefix 検索を行い、更新された global npm の entrypoint を選ぶ。
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            tokio::process::Command::new(&node)
                .arg(prefix_script)
                .current_dir(cwd)
                .envs(environment)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .context("npm prefix resolution timed out")??;
        if output.status.success() {
            let prefix =
                String::from_utf8(output.stdout).context("npm prefix is not valid Unicode")?;
            let prefix = Path::new(prefix.trim());
            if !prefix.is_absolute() {
                return fail(
                    "LAUNCHER_RESOLUTION_FAILED",
                    "npm returned a non-absolute installation prefix",
                );
            }
            let entry = script
                .file_name()
                .context("npm entrypoint has no filename")?;
            let candidate = prefix.join("node_modules/npm/bin").join(entry);
            if candidate.is_file() {
                script = candidate;
            }
        }
    }
    let script = fs::canonicalize(script).context("npm launcher entrypoint does not exist")?;
    if !script.is_file() {
        return fail("LAUNCHER_RESOLUTION_FAILED", "npm entrypoint is not a file");
    }
    Ok(Some((node, script)))
}

fn parse(text: &str, parent: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let Some(last) = lines.last() else {
        return unsupported();
    };
    let lower = text.to_ascii_lowercase();
    if lower.contains("set dp0=%~dp0") && lower.contains("set \"_prog=node\"") {
        let Some(position) = last.to_ascii_lowercase().rfind("\"%_prog%\"") else {
            return unsupported();
        };
        let tail = last[position + "\"%_prog%\"".len()..].trim();
        let Some(target) = tail
            .strip_suffix(" %*")
            .and_then(|tail| tail.strip_prefix('"'))
            .and_then(|tail| tail.strip_suffix('"'))
        else {
            return unsupported();
        };
        return Ok((relative_target(parent, target)?, None));
    }
    // Node.js 同梱の npm / npx は変数を使う別形式のランチャーを持つ。
    let variable = if last.eq_ignore_ascii_case("\"%NODE_EXE%\" \"%NPX_CLI_JS%\" %*") {
        "NPX_CLI_JS"
    } else if last.eq_ignore_ascii_case("\"%NODE_EXE%\" \"%NPM_CLI_JS%\" %*") {
        "NPM_CLI_JS"
    } else {
        return unsupported();
    };
    let Some(node) = assignment(&lines, "NODE_EXE") else {
        return unsupported();
    };
    if !node.eq_ignore_ascii_case("%~dp0\\node.exe") {
        return unsupported();
    }
    let Some(script) = assignment(&lines, variable) else {
        return unsupported();
    };
    let Some(prefix) = assignment(&lines, "NPM_PREFIX_JS") else {
        return unsupported();
    };
    Ok((
        relative_target(parent, script)?,
        Some(relative_target(parent, prefix)?),
    ))
}

fn assignment<'a>(lines: &[&'a str], name: &str) -> Option<&'a str> {
    let prefix = format!("SET \"{name}=");
    lines.iter().find_map(|line| {
        let start = line.get(..prefix.len())?;
        start
            .eq_ignore_ascii_case(&prefix)
            .then(|| line[prefix.len()..].strip_suffix('"'))
            .flatten()
    })
}

fn relative_target(parent: &Path, target: &str) -> Result<PathBuf> {
    let lower = target.to_ascii_lowercase();
    let offset = if lower.starts_with("%dp0%") || lower.starts_with("%~dp0") {
        5
    } else {
        return unsupported();
    };
    let relative = target[offset..].trim_start_matches(['\\', '/']);
    if relative.is_empty() || relative.contains(['%', '"', '\0']) {
        return unsupported();
    }
    let path = PathBuf::from(relative.replace('\\', "/"));
    if path.is_absolute()
        || !matches!(
            path.extension().and_then(OsStr::to_str),
            Some("js" | "cjs" | "mjs")
        )
    {
        return unsupported();
    }
    Ok(parent.join(path))
}

fn unsupported<T>() -> Result<T> {
    fail(
        "SHELL_REQUIRED",
        "Unsupported batch launcher; register cmd.exe or powershell explicitly",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ordinary_and_non_utf8_batches_fall_back_without_hiding_broken_node_shims() {
        let temp = tempfile::tempdir().unwrap();
        let shim = temp.path().join("start.bat");
        for bytes in [
            b"@echo off\r\necho hello\r\n".as_slice(),
            b"rem \x81\x40\r\n",
        ] {
            fs::write(&shim, bytes).unwrap();
            assert!(
                resolve(&shim, temp.path(), None, &BTreeMap::new())
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        fs::write(
            &shim,
            "SET dp0=%~dp0\nSET \"_prog=node\"\n\"%_prog%\" \"%dp0%\\missing.js\" %*\n",
        )
        .unwrap();
        assert!(
            resolve(&shim, temp.path(), None, &BTreeMap::new())
                .await
                .is_err()
        );
    }

    #[test]
    fn generated_node_launcher_keeps_spaces_and_relative_parent_components() {
        let temp = tempfile::tempdir().unwrap();
        let text = "SET dp0=%~dp0\nSET \"_prog=node\"\n\"%_prog%\" \"%dp0%\\..\\package with spaces\\bin.cjs\" %*\n";
        let (script, prefix) = parse(text, temp.path()).unwrap();
        assert_eq!(script, temp.path().join("../package with spaces/bin.cjs"));
        assert!(prefix.is_none());
        for tail in [
            "\"%_prog%\" \"%dp0%\\x.js\" %* & echo changed",
            "\"%_prog%\" \"%dp0%\\%NAME%\\x.js\" %*",
            "\"%_prog%\" --eval \"%dp0%\\x.js\" %*",
        ] {
            assert!(
                parse(
                    &format!("SET dp0=%~dp0\nSET \"_prog=node\"\n{tail}"),
                    temp.path()
                )
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn npm_entrypoint_and_adjacent_node_are_pinned_without_running_batch() {
        let temp = tempfile::tempdir().unwrap();
        let node = temp.path().join("node.exe");
        fs::write(&node, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&node, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let script = temp.path().join("package/cli.js");
        fs::create_dir_all(script.parent().unwrap()).unwrap();
        fs::write(&script, "fixture").unwrap();
        let shim = temp.path().join("server.cmd");
        fs::write(
            &shim,
            "SET dp0=%~dp0\nSET \"_prog=node\"\n\"%_prog%\" \"%dp0%\\package\\cli.js\" %*\n",
        )
        .unwrap();
        let resolved = resolve(&shim, temp.path(), None, &BTreeMap::new())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            resolved,
            (
                fs::canonicalize(node).unwrap(),
                fs::canonicalize(script).unwrap()
            )
        );
    }

    #[test]
    fn bundled_npx_selects_both_entrypoint_and_prefix_helper() {
        let temp = tempfile::tempdir().unwrap();
        let text = "SET \"NODE_EXE=%~dp0\\node.exe\"\nSET \"NPM_PREFIX_JS=%~dp0\\node_modules\\npm\\bin\\npm-prefix.js\"\nSET \"NPX_CLI_JS=%~dp0\\node_modules\\npm\\bin\\npx-cli.js\"\n\"%NODE_EXE%\" \"%NPX_CLI_JS%\" %*\n";
        let (script, prefix) = parse(text, temp.path()).unwrap();
        assert_eq!(script, temp.path().join("node_modules/npm/bin/npx-cli.js"));
        assert_eq!(
            prefix.unwrap(),
            temp.path().join("node_modules/npm/bin/npm-prefix.js")
        );
    }
}
