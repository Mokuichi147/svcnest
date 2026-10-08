use crate::{config::ServiceConfig, error::fail};
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
        if let Some(script) = &config.resolved_script {
            command.arg(script);
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
            // std::process は .cmd/.bat を暗黙に cmd.exe へ渡すため、明示した shell 以外は拒否する。
            if config
                .resolved_executable
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"))
            {
                return fail(
                    "SHELL_REQUIRED",
                    "Register cmd.exe or powershell explicitly to run a batch file",
                );
            }
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
        let mut child = command
            .spawn()
            .with_context(|| format!("Cannot spawn {}", config.resolved_executable.display()))?;
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
    let channels = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::interrupt()),
            signal(SignalKind::terminate()),
        )
    };
    #[cfg(windows)]
    let channels = (
        tokio::signal::windows::ctrl_c(),
        tokio::signal::windows::ctrl_break(),
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
