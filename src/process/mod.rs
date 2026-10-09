use crate::{config::ServiceConfig, error::fail, paths::readable_path};
use anyhow::{Context, Result};
use std::{
    process::{ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tokio::process::{Child, Command};

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
mod windows;

pub struct ProcessTree {
    pub child: Child,
    #[cfg(unix)]
    containment: unix::Group,
    #[cfg(windows)]
    containment: windows::Job,
    #[cfg(unix)]
    terminal: Option<unix::TerminalGuard>,
    armed: bool,
}

impl ProcessTree {
    pub fn spawn(config: &ServiceConfig, foreground: bool) -> Result<Self> {
        config.validate()?;
        let mut command = Command::new(&config.resolved_executable);
        command.args(&config.interpreter_args);
        if let Some(script) = &config.resolved_script {
            #[cfg(windows)]
            let script = if config.interpreter_args.is_empty() {
                script.to_owned()
            } else {
                crate::resolve::windows_shell::script_argument(script)
            };
            command.arg(script);
        }
        #[cfg(windows)]
        if config.resolved_script.is_some()
            && crate::resolve::windows_shell::is_powershell(&config.resolved_executable)
        {
            // 起動済み daemon の別バージョンの PowerShell 環境を混ぜない。
            // 保存したシェル環境や明示した環境は、続く envs で適用する。
            command
                .env_remove("PSModulePath")
                .env_remove("PSExecutionPolicyPreference");
        }
        command
            .args(&config.command[1..])
            .current_dir(&config.cwd)
            .envs(config.effective_environment()?)
            .kill_on_drop(true);
        if foreground {
            command
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        } else {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Threading::{
                CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED,
            };
            // .cmd/.bat は std::process がシステムの cmd.exe と専用の引数エスケープで起動する。
            // foreground は端末の Ctrl+C を直接受信できる、既存の console グループを使う。
            command.creation_flags(
                CREATE_SUSPENDED
                    | if foreground {
                        0
                    } else {
                        CREATE_NEW_PROCESS_GROUP
                    },
            );
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "Cannot spawn {}",
                readable_path(&config.resolved_executable).display()
            )
        })?;
        let pid = child.id().context("Spawned process has no PID")?;
        #[cfg(unix)]
        let containment = unix::Group::new(pid)?;
        #[cfg(windows)]
        let containment = match windows::Job::assign_and_resume(&child, pid) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.start_kill();
                return Err(error);
            }
        };
        // Unix では子を動かした後に失敗してもプロセスグループを取り残さない。
        let _ = &mut child;
        let mut tree = Self {
            child,
            containment,
            #[cfg(unix)]
            terminal: None,
            armed: true,
        };
        #[cfg(unix)]
        if foreground {
            tree.terminal = unix::TerminalGuard::attach(pid as i32)?;
        }
        let _ = &mut tree;
        Ok(tree)
    }

    pub fn refresh(&mut self) -> Result<()> {
        #[cfg(unix)]
        self.containment.refresh()?;
        Ok(())
    }

    pub async fn stop(&mut self, timeout: Duration, interrupted: bool) -> Result<ExitStatus> {
        self.refresh()?;
        self.containment.graceful(interrupted)?;
        let deadline = Instant::now() + timeout;
        loop {
            let exited = self.child.try_wait()?.is_some();
            if exited && !self.containment.alive()? {
                break;
            }
            if Instant::now() >= deadline {
                self.containment.force()?;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let status = self.child.wait().await?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.containment.alive()? {
            self.containment.force()?;
            if Instant::now() >= deadline {
                return fail(
                    "STOP_TIMEOUT",
                    "Process tree is still alive after forced termination; restart was refused",
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.armed = false;
        #[cfg(unix)]
        drop(self.terminal.take());
        Ok(status)
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.containment.force();
        }
    }
}

pub fn prepare_supervisor() -> Result<()> {
    #[cfg(target_os = "linux")]
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    #[cfg(windows)]
    windows::prepare_console();
    Ok(())
}

pub fn interrupt() -> impl std::future::Future<Output = ()> {
    // Future を poll する前に登録し、process spawn 中の割り込みも保持する。
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let channels = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        );
        async move {
            match channels {
                (Ok(mut interrupt), Ok(mut terminate)) => {
                    tokio::select! { _ = interrupt.recv() => (), _ = terminate.recv() => () }
                }
                (Ok(mut interrupt), Err(_)) => {
                    interrupt.recv().await;
                }
                (Err(_), Ok(mut terminate)) => {
                    terminate.recv().await;
                }
                (Err(_), Err(_)) => std::future::pending::<()>().await,
            }
        }
    }
    #[cfg(windows)]
    {
        WindowsInterrupt {
            ctrl_c: tokio::signal::windows::ctrl_c().ok(),
            ctrl_break: tokio::signal::windows::ctrl_break().ok(),
        }
    }
}

#[cfg(windows)]
struct WindowsInterrupt {
    ctrl_c: Option<tokio::signal::windows::CtrlC>,
    ctrl_break: Option<tokio::signal::windows::CtrlBreak>,
}

#[cfg(windows)]
impl std::future::Future for WindowsInterrupt {
    type Output = ();

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        // 最初の通知後も受信側を保持し、停止中の追加 Ctrl+C / Ctrl+Break が
        // Windows の既定ハンドラーで supervisor を強制終了しないようにする。
        let signals = self.get_mut();
        if signals
            .ctrl_c
            .as_mut()
            .is_some_and(|signal| signal.poll_recv(cx).is_ready())
            || signals
                .ctrl_break
                .as_mut()
                .is_some_and(|signal| signal.poll_recv(cx).is_ready())
        {
            std::task::Poll::Ready(())
        } else {
            std::task::Poll::Pending
        }
    }
}

pub fn exit_details(status: ExitStatus) -> (Option<i32>, Option<i32>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (status.code(), status.signal())
    }
    #[cfg(windows)]
    {
        (status.code(), None)
    }
}
