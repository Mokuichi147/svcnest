use crate::{error::fail, resolve::executable};
use anyhow::Result;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    mem::{size_of, zeroed},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf, Prefix},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW},
    },
};

pub fn powershell(cwd: &Path, path: Option<&OsStr>, explicit: Option<&str>) -> Result<PathBuf> {
    if let Some(command) = explicit {
        let shell = executable::resolve(command, cwd, path, Some(OsStr::new(".EXE")))?;
        if !is_powershell(&shell) {
            return fail(
                "INVALID_SHELL",
                "Select powershell.exe or pwsh.exe for a .ps1 script",
            );
        }
        return Ok(shell);
    }
    if let Some(shell) = current_shell().filter(|shell| is_powershell(shell)) {
        return Ok(shell);
    }
    for command in ["pwsh.exe", "powershell.exe"] {
        if let Ok(shell) = executable::resolve(command, cwd, path, None) {
            return Ok(shell);
        }
    }
    // PATH が限定されていても Windows 標準の PowerShell を利用できる。
    if let Some(root) = std::env::var_os("SystemRoot") {
        let shell = PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        if executable::is_executable(&shell) {
            return Ok(fs::canonicalize(shell)?);
        }
    }
    fail(
        "SHELL_NOT_FOUND",
        "Cannot find PowerShell; specify --shell with a powershell.exe or pwsh.exe path",
    )
}

pub(crate) fn is_powershell(path: &Path) -> bool {
    matches!(shell_name(path).as_str(), "powershell.exe" | "pwsh.exe")
}

pub fn environment(shell: &Path) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::new();
    if let Ok(policy) = std::env::var("PSExecutionPolicyPreference") {
        environment.insert("PSExecutionPolicyPreference".into(), policy);
    }
    // 異なる PowerShell のモジュールパスを混ぜず、選択したシェル自身の既定値を使う。
    if current_shell().as_deref() == Some(shell)
        && let Ok(path) = std::env::var("PSModulePath")
    {
        environment.insert("PSModulePath".into(), path);
    }
    environment
}

pub(crate) fn script_argument(path: &Path) -> PathBuf {
    // Windows PowerShell 5.1 は \\?\ 形式をリモート扱いし、RemoteSigned でも署名を要求する。
    // 保存したパスは保持し、PowerShell へ渡す時だけ通常のドライブ / UNC 表記へ戻す。
    let Some(Component::Prefix(prefix)) = path.components().next() else {
        return path.to_owned();
    };
    let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    match prefix.kind() {
        Prefix::VerbatimDisk(_) => PathBuf::from(OsString::from_wide(&wide[4..])),
        Prefix::VerbatimUNC(_, _) => {
            let mut normal = vec![b'\\' as u16, b'\\' as u16];
            normal.extend_from_slice(&wide[8..]);
            PathBuf::from(OsString::from_wide(&normal))
        }
        _ => path.to_owned(),
    }
}

fn shell_name(path: &Path) -> String {
    path.file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("")
        .to_ascii_lowercase()
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn process_image(pid: u32) -> Option<PathBuf> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let handle = OwnedHandle(handle);
    let mut buffer = vec![0u16; 32768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(handle.0, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    fs::canonicalize(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
    .ok()
}

fn current_shell() -> Option<PathBuf> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry: PROCESSENTRY32W = unsafe { zeroed() };
    entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
    let mut parents = HashMap::new();
    let mut found = unsafe { Process32FirstW(snapshot.0, &mut entry) };
    while found != 0 {
        parents.insert(entry.th32ProcessID, entry.th32ParentProcessID);
        found = unsafe { Process32NextW(snapshot.0, &mut entry) };
    }
    let mut pid = std::process::id();
    let mut seen = HashSet::new();
    // ラッパー越しでも直近のシェルを選ぶ。別のシェルを跨いで PowerShell を拾わない。
    while let Some(parent) = parents.get(&pid).copied() {
        if parent == 0 || !seen.insert(parent) {
            break;
        }
        if let Some(image) = process_image(parent)
            && matches!(
                shell_name(&image).as_str(),
                "powershell.exe"
                    | "pwsh.exe"
                    | "cmd.exe"
                    | "bash.exe"
                    | "sh.exe"
                    | "zsh.exe"
                    | "fish.exe"
                    | "nu.exe"
            )
        {
            return Some(image);
        }
        pid = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_shell_uses_the_registration_path_and_requires_powershell() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("shell with spaces");
        fs::create_dir(&bin).unwrap();
        for name in ["pwsh.EXE", "cmd.exe"] {
            fs::write(bin.join(name), "fixture").unwrap();
        }
        let path = std::env::join_paths([&bin]).unwrap();
        assert_eq!(
            powershell(temp.path(), Some(&path), Some("pwsh")).unwrap(),
            fs::canonicalize(bin.join("pwsh.EXE")).unwrap()
        );
        assert_eq!(
            crate::error::ServiceError::code(
                &powershell(temp.path(), Some(&path), Some("cmd.exe")).unwrap_err()
            ),
            "INVALID_SHELL"
        );
        assert!(powershell(temp.path(), Some(&path), Some("missing")).is_err());
    }
}
