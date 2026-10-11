//! ログインシェルの rc ファイルが export する環境変数を取得する。
//!
//! daemon は LaunchAgent / systemd user から起動するため、端末で読み込む rc ファイルの
//! 環境を持たない。起動のたびにシェルの種類に応じた方法で login / rc ファイルを読み込み、
//! そのシェルから svcnest 自身を実行して環境を受け取る。値は設定へ保存せず、
//! rc の出力や診断へ転記しない。

use crate::{error::fail, paths::readable_path, resolve};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    ffi::{CStr, OsStr, OsString},
    io::Write,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub type Environment = BTreeMap<OsString, OsString>;

pub const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT: usize = 4 << 20;
// シェル自身の状態を表し、対象の起動環境としては意味を持たない変数。
const SHELL_STATE: [&str; 3] = ["_", "PWD", "OLDPWD"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// sh / dash / ksh など。`-i -l` で profile と `$ENV` を読む。
    Posix,
    /// login では `.bashrc` を読まないため、profile の後に対話シェルを起動する。
    Bash,
    Zsh,
    Fish,
    /// `-l` は単独でしか指定できないため、コマンドを stdin から渡す。
    Csh,
    /// `-Login` で `/etc/profile` などを読み、`-Command` では `$PROFILE` も読む。
    PowerShell,
    /// 未知のシェル。`-i -l -c` と POSIX の引用を試す。
    Other,
}

/// シェルの種類を返す。tmux などの端末多重化ソフトはシェルとして扱わない。
pub fn kind(shell: &Path) -> Option<Kind> {
    let name = shell.file_name()?.to_string_lossy();
    let name = name.trim_start_matches('-');
    Some(match name {
        "bash" | "rbash" => Kind::Bash,
        "zsh" => Kind::Zsh,
        "fish" => Kind::Fish,
        "csh" | "tcsh" => Kind::Csh,
        "pwsh" | "pwsh-preview" | "powershell" => Kind::PowerShell,
        "sh" | "dash" | "ash" | "ksh" | "ksh93" | "mksh" | "lksh" | "oksh" | "pdksh" | "yash"
        | "posh" => Kind::Posix,
        "tmux" | "screen" | "byobu" | "zellij" => return None,
        _ => Kind::Other,
    })
}

/// 登録元の端末で検出したシェル、passwd のログインシェル、`SHELL`、`/bin/sh` の順に選ぶ。
pub fn select(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(shell) = configured {
        if !resolve::executable::is_executable(shell) {
            return fail(
                "SHELL_NOT_FOUND",
                format!(
                    "Shell {} no longer exists; set `shell` in the config, register again from another shell, or use --no-shell-env",
                    readable_path(shell).display()
                ),
            );
        }
        return Ok(shell.to_owned());
    }
    Ok(passwd_shell()
        .into_iter()
        .chain(std::env::var_os("SHELL").map(PathBuf::from))
        .find(|shell| {
            shell.is_absolute()
                && kind(shell).is_some()
                && resolve::executable::is_executable(shell)
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh")))
}

fn passwd_shell() -> Option<PathBuf> {
    let mut buffer = vec![0u8; 16 << 10];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() || entry.pw_shell.is_null() {
        return None;
    }
    let shell = unsafe { CStr::from_ptr(entry.pw_shell) }.to_bytes();
    (!shell.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(shell)))
}

/// 登録元の端末で動いている対話シェルを、親プロセスを辿って探す。
///
/// `sh -c` やスクリプトを実行中のシェルは飛ばし、引数がオプションだけのシェルを選ぶ。
/// パッケージ更新で変わる実体のパスではなく、同じファイルを指す `SHELL` や PATH 上の
/// パスを返す。
pub fn registering_shell(cwd: &Path, path: Option<&OsStr>) -> Option<PathBuf> {
    let mut pid = unsafe { libc::getppid() };
    for _ in 0..64 {
        if pid <= 1 {
            return None;
        }
        let process = inspect(pid)?;
        if is_known_shell(&process.executable) && is_interactive(&process.args) {
            return Some(stable_path(&process.executable, cwd, path));
        }
        pid = process.ppid;
    }
    None
}

// 親プロセスの検出では、名前だけで他のプログラムをシェルと誤認しない。
fn is_known_shell(executable: &Path) -> bool {
    kind(executable).is_some_and(|kind| kind != Kind::Other)
        || executable.file_name().is_some_and(|name| {
            matches!(
                name.to_str(),
                Some("nu" | "xonsh" | "elvish" | "osh" | "ysh")
            )
        })
}

fn is_interactive(args: &[OsString]) -> bool {
    args.iter().skip(1).all(|arg| {
        let arg = arg.to_string_lossy();
        arg.starts_with('-')
            && arg != "--command"
            && !arg.eq_ignore_ascii_case("-command")
            // `-c` や `-lc` のような短いオプションの組み合わせ。
            && !(!arg.starts_with("--") && arg.len() <= 4 && arg.contains('c'))
    })
}

fn stable_path(executable: &Path, cwd: &Path, path: Option<&OsStr>) -> PathBuf {
    let same = |candidate: &Path| {
        std::fs::canonicalize(candidate).ok() == std::fs::canonicalize(executable).ok()
    };
    let name = executable.file_name().map(|name| name.to_string_lossy());
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|shell| shell.is_absolute() && same(shell))
        .or_else(|| {
            resolve::executable::resolve(name.as_deref()?, cwd, path, None)
                .ok()
                .filter(|shell| same(shell))
        })
        .unwrap_or_else(|| executable.to_owned())
}

struct ProcessInfo {
    ppid: i32,
    executable: PathBuf,
    args: Vec<OsString>,
}

#[cfg(target_os = "linux")]
fn inspect(pid: i32) -> Option<ProcessInfo> {
    let root = PathBuf::from(format!("/proc/{pid}"));
    let stat = std::fs::read_to_string(root.join("stat")).ok()?;
    let ppid = stat[stat.rfind(')')? + 1..]
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    let args = std::fs::read(root.join("cmdline"))
        .ok()?
        .split(|&byte| byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| OsString::from_vec(arg.to_vec()))
        .collect();
    // 他ユーザーのプロセスなど、実体を読めない場合も親を辿り続ける。
    let executable = std::fs::read_link(root.join("exe")).unwrap_or_default();
    Some(ProcessInfo {
        ppid,
        executable,
        args,
    })
}

#[cfg(target_os = "macos")]
fn inspect(pid: i32) -> Option<ProcessInfo> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of_val(&info) as i32;
    if unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    } != size
    {
        return None;
    }
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    let executable = if length > 0 {
        PathBuf::from(OsStr::from_bytes(&path[..length as usize]))
    } else {
        PathBuf::new()
    };
    Some(ProcessInfo {
        ppid: info.pbi_ppid as i32,
        executable,
        args: macos_args(pid).unwrap_or_default(),
    })
}

#[cfg(target_os = "macos")]
fn macos_args(pid: i32) -> Option<Vec<OsString>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = 0usize;
    let sysctl = |buffer: *mut libc::c_void, size: &mut usize, mib: &mut [i32; 3]| unsafe {
        libc::sysctl(mib.as_mut_ptr(), 3, buffer, size, std::ptr::null_mut(), 0)
    };
    if sysctl(std::ptr::null_mut(), &mut size, &mut mib) != 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    if sysctl(buffer.as_mut_ptr().cast(), &mut size, &mut mib) != 0 {
        return None;
    }
    buffer.truncate(size);
    // argc、実行パス、NUL の詰め物、argv の順に並ぶ。
    let argc = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?) as usize;
    let rest = &buffer[4..];
    let rest = &rest[rest.iter().position(|&byte| byte == 0)?..];
    let rest = &rest[rest.iter().position(|&byte| byte != 0)?..];
    Some(
        rest.split(|&byte| byte == 0)
            .take(argc)
            .map(|arg| OsString::from_vec(arg.to_vec()))
            .collect(),
    )
}

struct Invocation {
    args: Vec<OsString>,
    stdin: Option<Vec<u8>>,
}

fn invocation(kind: Kind, shell: &Path, executable: &Path, nonce: &str) -> Invocation {
    let posix = format!(
        "exec {} __shell-env {nonce}",
        posix_quote(executable.as_os_str())
    );
    let args = |args: &[&str], script: String| Invocation {
        args: args
            .iter()
            .map(OsString::from)
            .chain([OsString::from(script)])
            .collect(),
        stdin: None,
    };
    match kind {
        Kind::Posix | Kind::Zsh | Kind::Other => args(&["-i", "-l", "-c"], posix),
        // 端末と同じく、ログイン時の profile と端末ごとの .bashrc の両方を読む。
        Kind::Bash => args(
            &["-l", "-c"],
            format!(
                "exec {} -i -c {}",
                posix_quote(shell.as_os_str()),
                posix_quote(OsStr::new(&posix))
            ),
        ),
        Kind::Fish => args(
            &["-i", "-l", "-c"],
            format!(
                "exec {} __shell-env {nonce}",
                fish_quote(executable.as_os_str())
            ),
        ),
        Kind::Csh => Invocation {
            args: vec!["-l".into()],
            stdin: Some(format!("{posix}\n").into_bytes()),
        },
        Kind::PowerShell => args(
            &["-Login", "-NoLogo", "-Command"],
            format!(
                "& {} __shell-env {nonce}",
                powershell_quote(executable.as_os_str())
            ),
        ),
    }
}

/// `shell` の login / rc ファイルを `cwd` で読み込み、export された環境を返す。
/// `executable` は `__shell-env` を実装する svcnest の実行ファイル。
pub async fn capture(shell: &Path, cwd: &Path, executable: &Path) -> Result<Environment> {
    let shown = readable_path(shell).display().to_string();
    let Some(kind) = kind(shell) else {
        return fail(
            "SHELL_ENV_FAILED",
            format!(
                "{shown} is not a shell; set `shell` in the config or register with --no-shell-env"
            ),
        );
    };
    let nonce = format!(
        "svcnest-env-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let invocation = invocation(kind, shell, executable, &nonce);
    let mut command = Command::new(shell);
    command
        .args(&invocation.args)
        .current_dir(cwd)
        .stdin(if invocation.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        // rc の警告などに秘密情報が含まれる可能性があるため、ログへ転記しない。
        .stderr(Stdio::null())
        .kill_on_drop(true);
    unsafe {
        // 制御端末から切り離し、対話シェルが foreground の端末グループを奪わないようにする。
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("Cannot start login shell {shown}"))?;
    let session = child.id().map(|pid| pid as i32);
    let mut stdout = child.stdout.take().context("Login shell stdout missing")?;
    let stdin = child.stdin.take();
    let result = tokio::time::timeout(TIMEOUT, async {
        if let (Some(mut stdin), Some(script)) = (stdin, invocation.stdin) {
            // rc が先に終了しても、環境を報告しなかったことは下の判定で扱う。
            let _ = stdin.write_all(&script).await;
        }
        let mut output = Vec::new();
        let mut chunk = [0u8; 8192];
        // rc が起動した background プロセスが stdout を保持しても、終端を読んだ時点で完了する。
        let environment = loop {
            if let Some(environment) = parse(&output, &nonce) {
                break Some(environment);
            }
            let read = stdout.read(&mut chunk).await?;
            if read == 0 || output.len() + read > MAX_OUTPUT {
                break None;
            }
            output.extend_from_slice(&chunk[..read]);
        };
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((environment, status))
    })
    .await;
    match result {
        Ok(Ok((Some(environment), _))) => Ok(environment),
        Ok(Ok((None, status))) => fail(
            "SHELL_ENV_FAILED",
            format!(
                "Login shell {shown} exited ({status}) before reporting its environment; \
                 check its rc files or register with --no-shell-env"
            ),
        ),
        Ok(Err(error)) => Err(error)
            .with_context(|| format!("Cannot read the environment of login shell {shown}")),
        Err(_) => {
            if let Some(session) = session {
                unsafe {
                    libc::kill(-session, libc::SIGKILL);
                }
            }
            let _ = child.kill().await;
            fail(
                "SHELL_ENV_TIMEOUT",
                format!(
                    "Login shell {shown} did not report its environment within {}s; \
                     check its rc files for prompts or register with --no-shell-env",
                    TIMEOUT.as_secs()
                ),
            )
        }
    }
}

/// `__shell-env` の本体。シェルから exec され、環境を NUL 区切りで出力する。
pub fn print(nonce: &str) -> Result<()> {
    let mut output = Vec::new();
    output.push(0);
    output.extend_from_slice(nonce.as_bytes());
    output.push(0);
    for (key, value) in std::env::vars_os() {
        output.extend_from_slice(key.as_bytes());
        output.push(b'=');
        output.extend_from_slice(value.as_bytes());
        output.push(0);
    }
    output.extend_from_slice(nonce.as_bytes());
    output.push(0);
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&output)?;
    stdout.flush()?;
    Ok(())
}

fn parse(output: &[u8], nonce: &str) -> Option<Environment> {
    let mut begin = vec![0];
    begin.extend_from_slice(nonce.as_bytes());
    begin.push(0);
    let start = output
        .windows(begin.len())
        .position(|window| window == begin.as_slice())?
        + begin.len();
    let mut environment = Environment::new();
    let mut rest = &output[start..];
    loop {
        let end = rest.iter().position(|&byte| byte == 0)?;
        let entry = &rest[..end];
        rest = &rest[end + 1..];
        if entry == nonce.as_bytes() {
            return Some(environment);
        }
        // Unix では先頭の '=' を名前に含めない。
        let Some(split) = entry.iter().skip(1).position(|&byte| byte == b'=') else {
            continue;
        };
        let (key, value) = entry.split_at(split + 1);
        if SHELL_STATE.iter().any(|name| key == name.as_bytes()) {
            continue;
        }
        environment.insert(
            OsString::from_vec(key.to_vec()),
            OsString::from_vec(value[1..].to_vec()),
        );
    }
}

fn posix_quote(value: &OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', r"'\''"))
}

fn fish_quote(value: &OsStr) -> String {
    format!(
        "'{}'",
        value
            .to_string_lossy()
            .replace('\\', r"\\")
            .replace('\'', r"\'")
    )
}

fn powershell_quote(value: &OsStr) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ignores_rc_output_and_shell_state() {
        let mut output = b"motd\0noise\n".to_vec();
        output.extend_from_slice(b"\0n1\0A=1\0B=x=y\0_=/bin/svcnest\0PWD=/tmp\0EMPTY=\0n1\0tail");
        let environment = parse(&output, "n1").unwrap();
        assert_eq!(
            environment,
            Environment::from([
                ("A".into(), "1".into()),
                ("B".into(), "x=y".into()),
                ("EMPTY".into(), "".into()),
            ])
        );
        assert!(parse(b"\0n1\0A=1\0", "n1").is_none());
    }

    #[test]
    fn quoting_follows_each_shell() {
        assert_eq!(posix_quote(OsStr::new("/a b/it's")), r"'/a b/it'\''s'");
        assert_eq!(fish_quote(OsStr::new(r"/a\b/it's")), r"'/a\\b/it\'s'");
        assert_eq!(powershell_quote(OsStr::new("/a/it's")), "'/a/it''s'");
    }

    #[test]
    fn shells_are_classified_by_name() {
        for (shell, expected) in [
            ("/bin/bash", Some(Kind::Bash)),
            ("/usr/bin/zsh", Some(Kind::Zsh)),
            ("-zsh", Some(Kind::Zsh)),
            ("/opt/homebrew/bin/fish", Some(Kind::Fish)),
            ("/bin/tcsh", Some(Kind::Csh)),
            ("/usr/bin/pwsh", Some(Kind::PowerShell)),
            ("/bin/dash", Some(Kind::Posix)),
            ("/usr/bin/nu", Some(Kind::Other)),
            ("/usr/bin/tmux", None),
        ] {
            assert_eq!(kind(Path::new(shell)), expected, "{shell}");
        }
    }

    #[test]
    fn invocations_load_login_and_interactive_files() {
        let run = |kind, shell: &str| {
            let invocation = invocation(kind, Path::new(shell), Path::new("/x/svcnest"), "n");
            (
                invocation
                    .args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
                invocation
                    .stdin
                    .map(|stdin| String::from_utf8(stdin).unwrap()),
            )
        };
        let posix = "exec '/x/svcnest' __shell-env n".to_owned();
        assert_eq!(
            run(Kind::Zsh, "/bin/zsh"),
            (
                vec!["-i".into(), "-l".into(), "-c".into(), posix.clone()],
                None
            )
        );
        assert_eq!(
            run(Kind::Bash, "/bin/bash"),
            (
                vec![
                    "-l".into(),
                    "-c".into(),
                    r"exec '/bin/bash' -i -c 'exec '\''/x/svcnest'\'' __shell-env n'".into()
                ],
                None
            )
        );
        assert_eq!(
            run(Kind::Csh, "/bin/tcsh"),
            (vec!["-l".into()], Some(format!("{posix}\n")))
        );
        assert_eq!(
            run(Kind::PowerShell, "/usr/bin/pwsh").0,
            [
                "-Login",
                "-NoLogo",
                "-Command",
                "& '/x/svcnest' __shell-env n"
            ]
        );
    }

    #[test]
    fn only_known_shells_are_detected_from_parents() {
        assert!(is_known_shell(Path::new("/usr/bin/zsh")));
        assert!(is_known_shell(Path::new("/usr/bin/nu")));
        assert!(!is_known_shell(Path::new("/usr/bin/claude-desktop")));
        assert!(!is_known_shell(Path::new("/usr/bin/tmux")));
        assert!(!is_known_shell(Path::new("")));
    }

    #[test]
    fn interactive_shells_have_only_options() {
        let args = |args: &[&str]| args.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(is_interactive(&args(&["-zsh"])));
        assert!(is_interactive(&args(&["/bin/bash", "--login"])));
        assert!(is_interactive(&args(&["pwsh", "-Login", "-NoLogo"])));
        assert!(!is_interactive(&args(&["zsh", "-c", "svcnest add"])));
        assert!(!is_interactive(&args(&["bash", "-lc", "svcnest add"])));
        assert!(!is_interactive(&args(&["bash", "script.sh"])));
        assert!(!is_interactive(&args(&["fish", "--command", "x"])));
        assert!(!is_interactive(&args(&["pwsh", "-Command", "x"])));
    }
}
