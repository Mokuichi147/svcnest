use crate::paths::{Paths, private_dir};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};

fn source_bytes(source: &Path) -> Result<Vec<u8>> {
    // 読み取り中の書き換えは拒否し、rename によるインストール更新は許可する。
    let mut file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .open(source)
        .with_context(|| format!("Cannot open runtime source {}", source.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn destination(paths: &Paths, bytes: &[u8]) -> PathBuf {
    let digest: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    paths.home.join("bin").join(digest).join("svcnest.exe")
}

pub fn executable_path(paths: &Paths, source: &Path) -> Result<PathBuf> {
    Ok(destination(paths, &source_bytes(source)?))
}

fn matches(path: &Path, bytes: &[u8]) -> Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file() && metadata.file_attributes() & 0x400 == 0,
        "Runtime executable must be a regular file: {}",
        path.display()
    );
    ensure!(
        source_bytes(path)? == bytes,
        "Runtime executable does not match its build hash: {}",
        path.display()
    );
    Ok(true)
}

pub fn prepare(paths: &Paths, source: &Path) -> Result<PathBuf> {
    let bytes = source_bytes(source)?;
    let path = destination(paths, &bytes);
    private_dir(&paths.home.join("bin"))?;
    let directory = path.parent().context("Runtime executable has no parent")?;
    private_dir(directory)?;
    if matches(&path, &bytes)? {
        return Ok(path);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    if let Err(error) = temporary.persist_noclobber(&path) {
        // 同時起動した別 CLI が先に配置した場合は、その同じビルドを再利用する。
        if !matches(&path, &bytes)? {
            return Err(error.error).context("Cannot publish runtime executable");
        }
    }
    Ok(path)
}

pub fn is_current_executable(paths: &Paths) -> Result<bool> {
    let current = fs::canonicalize(std::env::current_exe()?)?;
    Ok(current == executable_path(paths, &current)?)
}

pub async fn serve_registered(paths: &Paths, source: &Path) -> Result<i32> {
    let executable = prepare(paths, source)?;
    // Task Scheduler はコピー側の起動役を監視する。更新元の exe は読み取り後に解放する。
    // 登録後に更新元だけが更新されても、次のログオンでは新しいコピーを選ぶ。
    let status = tokio::process::Command::new(executable)
        .arg("--home")
        .arg(&paths.home)
        .args(["daemon", "serve"])
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .status()
        .await
        .context("Cannot launch the registered svcnest runtime")?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_are_immutable_and_identified_by_contents() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(temp.path().join("state"))).unwrap();
        let source = temp.path().join("installed.exe");
        fs::write(&source, b"first build, same package version").unwrap();
        let first = prepare(&paths, &source).unwrap();
        assert_eq!(prepare(&paths, &source).unwrap(), first);
        fs::write(&source, b"second build, same package version").unwrap();
        let second = prepare(&paths, &source).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            fs::read(first).unwrap(),
            b"first build, same package version"
        );
        assert_eq!(
            fs::read(second).unwrap(),
            b"second build, same package version"
        );
    }

    #[test]
    fn simultaneous_publishers_reuse_the_same_complete_build() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(temp.path().join("state"))).unwrap();
        let source = temp.path().join("installed.exe");
        let bytes = vec![17; 1024 * 1024];
        fs::write(&source, &bytes).unwrap();
        let copies = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| prepare(&paths, &source).unwrap()))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(copies.iter().all(|path| path == &copies[0]));
        assert_eq!(fs::read(&copies[0]).unwrap(), bytes);
    }

    #[test]
    fn damaged_builds_are_rejected_without_overwriting_them() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(temp.path().join("state"))).unwrap();
        let source = temp.path().join("installed.exe");
        fs::write(&source, b"expected build").unwrap();
        let cached = prepare(&paths, &source).unwrap();
        fs::write(&cached, b"damaged build").unwrap();
        assert!(prepare(&paths, &source).is_err());
        assert_eq!(fs::read(cached).unwrap(), b"damaged build");
    }
}
