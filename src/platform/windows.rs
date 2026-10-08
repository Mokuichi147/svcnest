use crate::{
    paths::Paths,
    platform::{checked_command, label, xml_escape},
};
use anyhow::{Context, Result};
use std::{
    ffi::c_void,
    fs::File,
    mem::{size_of, zeroed},
    os::windows::{ffi::OsStrExt, io::AsRawHandle},
    path::Path,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, LocalFree},
    Security::{
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        },
        GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
    System::{
        Pipes::{GetNamedPipeClientProcessId, GetNamedPipeServerProcessId},
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
        },
    },
};

pub fn user_sid() -> Result<String> {
    sid_of_process(unsafe { GetCurrentProcess() })
}

pub fn prevent_stdio_inheritance() -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation},
        System::Console::{GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE},
    };

    // 元の標準ハンドルが daemon や対象プロセスに残ると、CLI 終了後も
    // 呼び出し元のパイプが EOF にならない。明示した stdio は Command が複製する。
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = unsafe { GetStdHandle(id) };
        if !handle.is_null()
            && handle != INVALID_HANDLE_VALUE
            && unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("Cannot prevent standard handle inheritance");
        }
    }
    Ok(())
}

pub fn user_runtime_directory() -> Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath},
    };
    unsafe {
        let mut path = null_mut();
        let result = SHGetKnownFolderPath(&FOLDERID_LocalAppData, 0, null_mut(), &mut path);
        if result < 0 {
            CoTaskMemFree(path.cast());
            return Err(anyhow::anyhow!(
                "Cannot resolve the current user's local data directory: {result:#x}"
            ));
        }
        let mut length = 0;
        while *path.add(length) != 0 {
            length += 1;
        }
        let root = std::ffi::OsString::from_wide(std::slice::from_raw_parts(path, length));
        CoTaskMemFree(path.cast());
        Ok(std::path::PathBuf::from(root)
            .join("svcnest")
            .join("singleton"))
    }
}

fn sid_of_process(process: HANDLE) -> Result<String> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| {
            let mut length = 0;
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length);
            let mut buffer = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
            if GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                length,
                &mut length,
            ) == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
            let mut sid = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let mut count = 0;
            while *sid.add(count) != 0 {
                count += 1;
            }
            let text = String::from_utf16(std::slice::from_raw_parts(sid, count));
            LocalFree(sid.cast());
            Ok(text?)
        })();
        CloseHandle(token);
        result
    }
}

pub(crate) fn verify_pipe_peer(pipe: &impl AsRawHandle, server: bool) -> Result<()> {
    let handle = pipe.as_raw_handle().cast();
    let mut pid = 0;
    let valid = unsafe {
        if server {
            GetNamedPipeServerProcessId(handle, &mut pid)
        } else {
            GetNamedPipeClientProcessId(handle, &mut pid)
        }
    };
    if valid == 0 {
        return crate::error::fail("IPC_ACCESS_DENIED", "Cannot identify the named pipe peer");
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return crate::error::fail("IPC_ACCESS_DENIED", "Cannot verify the named pipe peer");
    }
    let sid = sid_of_process(process);
    unsafe {
        CloseHandle(process);
    }
    if sid? != user_sid()? {
        return crate::error::fail(
            "IPC_ACCESS_DENIED",
            "Named pipe peer is not the current user",
        );
    }
    Ok(())
}

pub fn private_pipe(
    name: &str,
    first: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    let sddl = format!("D:P(A;;GA;;;{})", user_sid()?);
    let wide = std::ffi::OsStr::new(&sddl)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast::<c_void>(),
            );
        LocalFree(descriptor);
        Ok(result?)
    }
}

pub fn private_directory(path: &Path) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    if std::fs::symlink_metadata(path)?.file_attributes() & 0x400 != 0 {
        return crate::error::fail(
            "UNSAFE_DIRECTORY",
            "Storage directories must not be reparse points",
        );
    }
    let sddl = format!("D:P(A;OICI;FA;;;{})", user_sid()?);
    let wide = std::ffi::OsStr::new(&sddl)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut name = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        let result = (|| {
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = null_mut();
            if GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let code = SetNamedSecurityInfoW(
                name.as_mut_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            );
            if code != 0 {
                return Err(std::io::Error::from_raw_os_error(code as i32).into());
            }
            Ok(())
        })();
        LocalFree(descriptor);
        result
    }
}

pub fn same_file(a: &File, b: &File) -> Result<bool> {
    let info = |file: &File| -> Result<BY_HANDLE_FILE_INFORMATION> {
        let mut info = unsafe { zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(info)
    };
    let a = info(a)?;
    let b = info(b)?;
    Ok((a.dwVolumeSerialNumber, a.nFileIndexHigh, a.nFileIndexLow)
        == (b.dwVolumeSerialNumber, b.nFileIndexHigh, b.nFileIndexLow))
}

pub fn quote_arg(arg: &str) -> String {
    let mut output = String::from("\"");
    let mut slashes = 0;
    for character in arg.chars() {
        if character == '\\' {
            slashes += 1;
            continue;
        }
        if character == '"' {
            output.push_str(&"\\".repeat(slashes * 2 + 1));
            output.push('"');
        } else {
            output.push_str(&"\\".repeat(slashes));
            output.push(character);
        }
        slashes = 0;
    }
    output.push_str(&"\\".repeat(slashes * 2));
    output.push('"');
    output
}

pub fn render(paths: &Paths, executable: &Path) -> Result<String> {
    let runtime = crate::runtime::executable_path(paths, executable)?;
    render_runtime(paths, &runtime, executable)
}

pub(crate) fn render_runtime(paths: &Paths, runtime: &Path, source: &Path) -> Result<String> {
    let sid = user_sid()?;
    let args = format!(
        "--home {} daemon serve --source-executable {}",
        quote_arg(&paths.home.to_string_lossy()),
        quote_arg(&source.to_string_lossy())
    );
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Task version=\"1.2\" xmlns=\"http://schemas.microsoft.com/windows/2004/02/mit/task\">\n  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId></LogonTrigger></Triggers>\n  <Principals><Principal id=\"User\"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>\n  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowStartOnDemand>true</AllowStartOnDemand><StartWhenAvailable>true</StartWhenAvailable><ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings>\n  <Actions Context=\"User\"><Exec><Command>{}</Command><Arguments>{}</Arguments><WorkingDirectory>{}</WorkingDirectory></Exec></Actions>\n</Task>\n",
        xml_escape(&runtime.to_string_lossy()),
        xml_escape(&args),
        xml_escape(&paths.home.to_string_lossy())
    ))
}

pub(crate) fn registration_bytes(definition: &str) -> Vec<u8> {
    // schtasks /XML に渡すファイルは、宣言と一致する BOM 付き UTF-16LE にする。
    // dry-run の標準出力は UTF-8 のまま、登録ファイルだけを変換する。
    let definition = definition.replacen("encoding=\"UTF-8\"", "encoding=\"UTF-16\"", 1);
    [0xff, 0xfe]
        .into_iter()
        .chain(definition.encode_utf16().flat_map(u16::to_le_bytes))
        .collect()
}

pub async fn install(paths: &Paths, path: &Path) -> Result<()> {
    checked_command(
        "schtasks.exe",
        &[
            "/Create".into(),
            "/TN".into(),
            label(paths).into(),
            "/XML".into(),
            path.as_os_str().to_owned(),
            "/F".into(),
        ],
    )
    .await?;
    Ok(())
}
pub async fn uninstall(paths: &Paths) -> Result<()> {
    checked_command(
        "schtasks.exe",
        &[
            "/Delete".into(),
            "/TN".into(),
            label(paths).into(),
            "/F".into(),
        ],
    )
    .await?;
    Ok(())
}
pub async fn status(paths: &Paths) -> Result<String> {
    checked_command(
        "schtasks.exe",
        &[
            "/Query".into(),
            "/TN".into(),
            label(paths).into(),
            "/XML".into(),
        ],
    )
    .await
}
