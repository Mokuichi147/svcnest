use crate::{error::fail, paths::readable_path};
use anyhow::{Context, Result};
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

pub fn working_directory(input: Option<&Path>) -> Result<PathBuf> {
    let path = match input {
        Some(path) => path.to_owned(),
        None => std::env::current_dir()?,
    };
    let path = fs::canonicalize(&path).with_context(|| {
        format!(
            "Working directory does not exist: {}",
            readable_path(&path).display()
        )
    })?;
    if !path.is_dir() {
        return fail("INVALID_CWD", "Working directory is not a directory");
    }
    Ok(path)
}

pub fn resolve(
    command: &str,
    cwd: &Path,
    path: Option<&OsStr>,
    pathext: Option<&OsStr>,
) -> Result<PathBuf> {
    let input = Path::new(command);
    if input.is_absolute() || command.contains('/') || command.contains('\\') {
        let candidate = if input.is_absolute() {
            input.to_owned()
        } else {
            cwd.join(input)
        };
        if let Some(path) = find_candidate(&candidate, pathext) {
            return Ok(path);
        }
    } else if let Some(path) = path {
        for dir in std::env::split_paths(path) {
            let dir = if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            };
            if let Some(path) = find_candidate(&dir.join(command), pathext) {
                return Ok(path);
            }
        }
    }
    fail(
        "EXECUTABLE_NOT_FOUND",
        format!("Cannot find executable '{command}' in the registration environment"),
    )
}

fn find_candidate(path: &Path, pathext: Option<&OsStr>) -> Option<PathBuf> {
    #[cfg(not(windows))]
    let _ = pathext;
    #[cfg(windows)]
    {
        if path.extension().is_none() {
            let extensions = pathext
                .and_then(OsStr::to_str)
                .unwrap_or(".COM;.EXE;.BAT;.CMD");
            for extension in extensions.split(';').filter(|e| !e.is_empty()) {
                let mut candidate = path.as_os_str().to_owned();
                candidate.push(extension);
                let candidate = PathBuf::from(candidate);
                if is_executable(&candidate) {
                    return absolute_executable(&candidate);
                }
            }
        }
    }
    is_executable(path)
        .then(|| absolute_executable(path))
        .flatten()
}

fn absolute_executable(path: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        fs::canonicalize(path).ok()
    }
    // 最後の symlink は解決しない。rustup の cargo などは実行名で動作を切り替える。
    #[cfg(not(windows))]
    {
        let parent = fs::canonicalize(path.parent()?).ok()?;
        Some(parent.join(path.file_name()?))
    }
}

pub fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        metadata.permissions().mode() & 0o111 != 0
            && unsafe { libc::access(path.as_ptr(), libc::X_OK) } == 0
    }
    #[cfg(windows)]
    {
        true
    }
}
