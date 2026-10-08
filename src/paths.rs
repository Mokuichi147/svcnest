#[cfg(unix)]
use crate::error::fail;
use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub configs: PathBuf,
    pub logs: PathBuf,
    pub runtime: PathBuf,
    pub singleton: PathBuf,
    #[cfg(windows)]
    pub user_sid: String,
}

impl Paths {
    pub fn discover(home: Option<PathBuf>) -> Result<Self> {
        let home = home
            .or_else(|| std::env::var_os("SVCNEST_HOME").map(PathBuf::from))
            .map(Ok)
            .unwrap_or_else(default_home)?;
        let home = if home.is_absolute() {
            home
        } else {
            std::env::current_dir()?.join(home)
        };
        private_dir(&home)?;
        let home = fs::canonicalize(home)?;
        #[cfg(unix)]
        let runtime = PathBuf::from("/tmp").join(format!(
            "svcnest-{}-{:016x}",
            unsafe { libc::geteuid() },
            path_hash(&home)
        ));
        #[cfg(windows)]
        let runtime = home.join("runtime");
        #[cfg(unix)]
        let singleton =
            PathBuf::from("/tmp").join(format!("svcnest-user-{}", unsafe { libc::geteuid() }));
        #[cfg(windows)]
        let singleton = crate::platform::windows::user_runtime_directory()?;
        let paths = Self {
            configs: home.join("services"),
            logs: home.join("logs"),
            home,
            runtime,
            singleton,
            #[cfg(windows)]
            user_sid: crate::platform::windows::user_sid()?,
        };
        for dir in [
            &paths.configs,
            &paths.logs,
            &paths.runtime,
            &paths.singleton,
        ] {
            private_dir(dir)?;
        }
        Ok(paths)
    }

    pub fn config(&self, name: &str) -> PathBuf {
        self.configs.join(format!("svc-{name}.toml"))
    }
    pub fn log(&self, name: &str) -> PathBuf {
        self.logs.join(format!("svc-{name}.log"))
    }
    pub fn service_lock(&self, name: &str) -> PathBuf {
        self.runtime.join(format!("svc-{name}.lock"))
    }
    pub fn daemon_lock(&self) -> PathBuf {
        self.singleton.join("daemon.lock")
    }
    pub fn status(&self, name: &str) -> PathBuf {
        self.runtime.join(format!("svc-{name}.json"))
    }
    #[cfg(unix)]
    pub fn endpoint(&self) -> PathBuf {
        self.runtime.join("daemon.sock")
    }
    #[cfg(windows)]
    pub fn endpoint(&self) -> String {
        format!(
            r"\\.\pipe\svcnest-{}-{:016x}",
            self.user_sid,
            path_hash(&self.home)
        )
    }
}

fn default_home() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(|p| PathBuf::from(p).join("svcnest"))
            .context("LOCALAPPDATA is not set")
    }
    #[cfg(unix)]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        #[cfg(target_os = "macos")]
        {
            Ok(home.join("Library/Application Support/svcnest"))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(".config"))
                .join("svcnest"))
        }
    }
}

pub fn path_hash(path: &Path) -> u64 {
    // バージョンが変わっても同じ IPC 名になるように固定のハッシュを使う。
    path.as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

pub fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        match builder.create(path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot create {}", path.display()));
            }
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return fail(
                "UNSAFE_DIRECTORY",
                format!(
                    "Directory is not owned by the current user: {}",
                    path.display()
                ),
            );
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        fs::create_dir_all(path)?;
        crate::platform::windows::private_directory(path)?;
    }
    Ok(())
}

pub struct Lock {
    _file: File,
}

impl Lock {
    pub fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("File has no parent directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    #[cfg(windows)]
    persist_windows(temp, path)?;
    #[cfg(not(windows))]
    temp.persist(path).map_err(|e| e.error)?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn persist_windows(temp: tempfile::NamedTempFile, path: &Path) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr::null, thread, time::Duration};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
        ReplaceFileW, SetFileAttributesW,
    };

    // NamedTempFile は FILE_FLAG_DELETE_ON_CLOSE で作られるため、置換 API を呼ぶ前に
    // ファイルハンドルを閉じる。ReplaceFileW は既存ファイルを一つの操作で置換するので、
    // fs::rename のように読者から一瞬だけ対象が消える窓を作らない。
    let temp = temp.into_temp_path();
    let wide = |value: &Path| {
        value
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let temporary = wide(&temp);
    let destination = wide(path);
    if unsafe { SetFileAttributesW(temporary.as_ptr(), FILE_ATTRIBUTE_NORMAL) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut last_error = None;
    for _ in 0..500 {
        let replaced = if path.is_file() {
            unsafe {
                ReplaceFileW(
                    destination.as_ptr(),
                    temporary.as_ptr(),
                    null(),
                    0,
                    null(),
                    null(),
                )
            }
        } else {
            unsafe {
                MoveFileExW(
                    temporary.as_ptr(),
                    destination.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            }
        };
        if replaced != 0 {
            // 置換後の一時パスは存在しない。TempPath の cleanup は不要であり、
            // 念のため名前を解放して終了する。
            std::mem::forget(temp);
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(5 | 32 | 33)) {
            return Err(error.into());
        }
        last_error = Some(error);
        thread::sleep(Duration::from_millis(2));
    }
    Err(last_error
        .unwrap_or_else(|| std::io::Error::other("Windows file replacement timed out"))
        .into())
}
