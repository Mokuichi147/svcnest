use std::{fs, process::Stdio, time::Duration};
use svcnest::{
    config::{self, RestartPolicy, ServiceConfig},
    ipc::{self, RuntimeStatus, ServiceState},
    logging::{LOG_GENERATIONS, MAX_LOG_BYTES, generation},
    paths::Paths,
    runner::Control,
};
use tokio::{io::BufReader, process::Command};

const CLI: &str = env!("CARGO_BIN_EXE_svcnest");

#[test]
fn fixture() {
    if let Some(gate) = std::env::var_os("SVCNEST_DIAGNOSTIC_GATE") {
        while !std::path::Path::new(&gate).exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    if let Ok(code) = std::env::var("SVCNEST_DIAGNOSTIC_EXIT_CODE") {
        std::process::exit(code.parse().unwrap());
    }
}

#[cfg(windows)]
#[test]
fn interrupt_fixture() {
    use windows_sys::Win32::System::Console::{
        AttachConsole, CTRL_BREAK_EVENT, FreeConsole, GenerateConsoleCtrlEvent,
        SetConsoleCtrlHandler,
    };
    let Ok(pid) = std::env::var("SVCNEST_DIAGNOSTIC_INTERRUPT_PID") else {
        return;
    };
    unsafe extern "system" fn ignore_signal(_: u32) -> i32 {
        1
    }
    unsafe {
        FreeConsole();
        assert_ne!(AttachConsole(pid.parse().unwrap()), 0);
        assert_ne!(SetConsoleCtrlHandler(Some(ignore_signal), 1), 0);
        assert_ne!(GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, 0), 0);
    }
    std::thread::sleep(Duration::from_millis(100));
    unsafe { FreeConsole() };
}

struct RunnerSandbox {
    temp: tempfile::TempDir,
    paths: Paths,
}

impl RunnerSandbox {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::discover(Some(temp.path().join("state"))).unwrap();
        Self { temp, paths }
    }

    fn config(&self, code: i32, restart: RestartPolicy) -> ServiceConfig {
        let probe = std::env::current_exe().unwrap();
        ServiceConfig {
            version: 1,
            name: "api".into(),
            description: String::new(),
            cwd: fs::canonicalize(self.temp.path()).unwrap(),
            command: vec![
                probe.to_string_lossy().into_owned(),
                "--exact".into(),
                "fixture".into(),
                "--nocapture".into(),
            ],
            resolved_executable: probe,
            resolved_script: None,
            interpreter_args: Vec::new(),
            interpreter_environment: Default::default(),
            enabled: true,
            restart,
            stop_timeout_ms: 300,
            env_file: None,
            environment: [("SVCNEST_DIAGNOSTIC_EXIT_CODE".into(), code.to_string())].into(),
        }
    }

    fn runner(&self, config: &ServiceConfig) -> tokio::process::Child {
        config::save(&self.paths, config).unwrap();
        let mut command = Command::new(CLI);
        command
            .arg("--home")
            .arg(&self.paths.home)
            .args(["__runner", "api"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        command.spawn().unwrap()
    }

    fn logs(&self) -> String {
        fs::read_to_string(self.paths.log("api")).unwrap()
    }

    fn silent_config(&self, restart: RestartPolicy) -> ServiceConfig {
        let mut config = self.config(37, restart);
        let gate = self.temp.path().join("finish");
        #[cfg(windows)]
        {
            let batch = self.temp.path().join("silent.cmd");
            fs::write(
                &batch,
                format!(
                    "@echo off\r\n:wait\r\nif exist \"{}\" exit /b 37\r\n\"%SystemRoot%\\System32\\ping.exe\" -n 2 127.0.0.1 >nul\r\ngoto wait\r\n",
                    gate.display()
                ),
            )
            .unwrap();
            config.command = vec![batch.to_string_lossy().into_owned()];
            config.resolved_executable = fs::canonicalize(batch).unwrap();
        }
        #[cfg(unix)]
        {
            config.resolved_executable = fs::canonicalize("/bin/sh").unwrap();
            config.command = vec![
                "/bin/sh".into(),
                "-c".into(),
                "while [ ! -e \"$SVCNEST_DIAGNOSTIC_GATE\" ]; do sleep 0.05; done; exit 37".into(),
            ];
            config.environment.insert(
                "SVCNEST_DIAGNOSTIC_GATE".into(),
                gate.to_string_lossy().into_owned(),
            );
        }
        config
    }

    fn block_log_rotation(&self, headroom: u64) {
        fs::File::create(self.paths.log("api"))
            .unwrap()
            .set_len(MAX_LOG_BYTES - headroom)
            .unwrap();
        // 通常のファイル削除で除去できないディレクトリを置き、確実にローテーションを失敗させる。
        fs::create_dir(generation(&self.paths.log("api"), LOG_GENERATIONS - 1)).unwrap();
    }

    async fn inspect(&self, args: &[&str]) -> String {
        let output = Command::new(CLI)
            .arg("--home")
            .arg(&self.paths.home)
            .current_dir(self.temp.path())
            .args(args)
            .output()
            .await
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        String::from_utf8(output.stdout).unwrap()
    }
}

impl Drop for RunnerSandbox {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = fs::remove_dir_all(&self.paths.runtime);
    }
}

#[tokio::test]
async fn natural_exits_have_lifecycle_logs_and_visible_offline_exit_codes() {
    tokio::time::timeout(Duration::from_secs(20), async {
        for code in [0, 7] {
            let sandbox = RunnerSandbox::new();
            let mut runner = sandbox.runner(&sandbox.config(code, RestartPolicy::OnFailure));
            let mut control = runner.stdin.take().unwrap();
            let mut reader = BufReader::new(runner.stdout.take().unwrap());
            let expected_state = if code == 0 {
                ServiceState::Stopped
            } else {
                ServiceState::Backoff
            };
            let status = loop {
                let status: RuntimeStatus = ipc::read_frame(&mut reader).await.unwrap().unwrap();
                if status.state == expected_state {
                    break status;
                }
            };
            assert_eq!(status.last_exit_code, Some(code));
            if code != 0 {
                ipc::write_frame(&mut control, &Control::Stop)
                    .await
                    .unwrap();
            }
            assert!(runner.wait().await.unwrap().success());
            let logs = sandbox.logs();
            assert!(logs.contains("svcnest | Started: pid="));
            assert!(logs.contains(&format!("Exited: code={code} signal=- uptime=")));
            if code == 0 {
                assert!(logs.contains("stopped: cause=exit-success restart=on-failure"));
                assert!(!logs.contains("Restart scheduled:"));
            } else {
                assert!(logs.contains("Restart scheduled: delay=1s restart=on-failure"));
                assert!(logs.contains("stopped: cause=stop-requested"));
            }
            assert_eq!(logs.matches("Started: pid=").count(), 1);
            let detailed = sandbox.inspect(&["status", "api"]).await;
            assert!(detailed.contains(&format!("Last exit:     {code}")));
            let table = sandbox.inspect(&["list"]).await;
            let columns: Vec<_> = table.lines().nth(1).unwrap().split_whitespace().collect();
            assert!(table.contains("LAST EXIT"));
            assert_eq!(columns[4], code.to_string());
            let json = sandbox.inspect(&["status", "api", "--json"]).await;
            let snapshot: ipc::Snapshot = serde_json::from_str(&json).unwrap();
            assert_eq!(snapshot.services[0].runtime.last_exit_code, Some(code));
            assert_eq!(snapshot.services[0].runtime.state, ServiceState::Stopped);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failure_without_restart_and_signal_exit_remain_visible() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let sandbox = RunnerSandbox::new();
        let mut runner = sandbox.runner(&sandbox.config(7, RestartPolicy::Never));
        let _control = runner.stdin.take().unwrap();
        assert!(runner.wait().await.unwrap().success());
        assert!(
            sandbox
                .logs()
                .contains("failed: cause=exit-failure restart=never")
        );
        assert!(
            sandbox
                .inspect(&["status", "api"])
                .await
                .contains("Last exit:     7")
        );
        let status = RuntimeStatus {
            state: ServiceState::Failed,
            last_exit_signal: Some(15),
            ..Default::default()
        };
        svcnest::paths::atomic_write(
            &sandbox.paths.status("api"),
            &serde_json::to_vec(&status).unwrap(),
        )
        .unwrap();
        assert!(
            sandbox
                .inspect(&["status", "api"])
                .await
                .contains("Last exit:     signal:15")
        );
        assert!(sandbox.inspect(&["list"]).await.contains("signal:15"));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn backoff_cancellation_saves_stopped_state_and_the_actual_cause() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for interrupted in [false, true] {
            let sandbox = RunnerSandbox::new();
            let mut runner = sandbox.runner(&sandbox.config(7, RestartPolicy::OnFailure));
            let control = runner.stdin.take().unwrap();
            let mut reader = BufReader::new(runner.stdout.take().unwrap());
            loop {
                let status: RuntimeStatus = ipc::read_frame(&mut reader).await.unwrap().unwrap();
                if status.state == ServiceState::Backoff {
                    break;
                }
            }
            let cause = if interrupted {
                #[cfg(unix)]
                assert_eq!(
                    unsafe { libc::kill(runner.id().unwrap() as i32, libc::SIGTERM) },
                    0
                );
                #[cfg(windows)]
                {
                    let output = Command::new(std::env::current_exe().unwrap())
                        .args(["--exact", "interrupt_fixture", "--nocapture"])
                        .env(
                            "SVCNEST_DIAGNOSTIC_INTERRUPT_PID",
                            runner.id().unwrap().to_string(),
                        )
                        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
                        .output()
                        .await
                        .unwrap();
                    assert!(output.status.success(), "{:?}", output);
                }
                "interrupted"
            } else {
                drop(control);
                "control-input-closed"
            };
            assert!(runner.wait().await.unwrap().success());
            let status: RuntimeStatus =
                serde_json::from_slice(&fs::read(sandbox.paths.status("api")).unwrap()).unwrap();
            assert_eq!(status.state, ServiceState::Stopped);
            assert_eq!(status.reason, None);
            assert_eq!(status.last_exit_code, Some(7));
            assert_eq!(status.restarts, 0);
            assert!(sandbox.logs().contains(&format!("stopped: cause={cause}")));
            assert_eq!(sandbox.logs().matches("Started: pid=").count(), 1);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn diagnostic_log_failure_at_startup_stops_the_target_and_notifies_failure() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let sandbox = RunnerSandbox::new();
        sandbox.block_log_rotation(0);
        let mut runner = sandbox.runner(&sandbox.silent_config(RestartPolicy::OnFailure));
        let _control = runner.stdin.take().unwrap();
        let mut reader = BufReader::new(runner.stdout.take().unwrap());
        let mut states = Vec::new();
        while let Some(status) = ipc::read_frame::<RuntimeStatus>(&mut reader).await.unwrap() {
            states.push(status);
        }
        assert!(runner.wait().await.unwrap().success());
        assert!(
            states
                .iter()
                .any(|status| status.state == ServiceState::Stopping)
        );
        let final_status = states.last().unwrap();
        assert_eq!(final_status.state, ServiceState::Failed);
        assert_eq!(final_status.reason.as_deref(), Some("log-error"));
        assert!(final_status.last_exit_code.is_some() || final_status.last_exit_signal.is_some());
        assert_eq!(final_status.pid, None);
        assert_eq!(final_status.restarts, 0);
        let stored: RuntimeStatus =
            serde_json::from_slice(&fs::read(sandbox.paths.status("api")).unwrap()).unwrap();
        assert_eq!(stored.reason, final_status.reason);
        assert_eq!(stored.last_exit_code, final_status.last_exit_code);
        assert_eq!(stored.last_exit_signal, final_status.last_exit_signal);
        assert!(
            svcnest::paths::Lock::try_acquire(&sandbox.paths.service_lock("api"))
                .unwrap()
                .is_some()
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn diagnostic_log_failures_after_exit_preserve_the_targets_code_and_notify_failure() {
    tokio::time::timeout(Duration::from_secs(15), async {
        // 起動行だけ、終了行まで、再起動予約行までが収まる容量を残す。
        // stdout/stderr が空なので、失敗するのは診断ログの書き込みだけになる。
        for (headroom, restart, cancel_backoff) in [
            (128, RestartPolicy::OnFailure, false),
            (200, RestartPolicy::Never, false),
            (200, RestartPolicy::OnFailure, false),
            (300, RestartPolicy::OnFailure, true),
        ] {
            let sandbox = RunnerSandbox::new();
            sandbox.block_log_rotation(headroom);
            let mut runner = sandbox.runner(&sandbox.silent_config(restart));
            let mut control = runner.stdin.take().unwrap();
            let mut reader = BufReader::new(runner.stdout.take().unwrap());
            let mut final_status = None;
            let mut backoff_seen = false;
            while let Some(status) = ipc::read_frame::<RuntimeStatus>(&mut reader).await.unwrap() {
                if status.state == ServiceState::Running {
                    fs::write(sandbox.temp.path().join("finish"), b"finish").unwrap();
                }
                if status.state == ServiceState::Backoff {
                    assert!(cancel_backoff, "Unexpected restart: headroom={headroom}");
                    backoff_seen = true;
                    ipc::write_frame(&mut control, &Control::Stop)
                        .await
                        .unwrap();
                }
                final_status = Some(status);
            }
            assert!(
                runner.wait().await.unwrap().success(),
                "headroom={headroom}"
            );
            let final_status = final_status.unwrap();
            assert_eq!(
                final_status.state,
                ServiceState::Failed,
                "headroom={headroom}"
            );
            assert_eq!(final_status.reason.as_deref(), Some("log-error"));
            assert_eq!(final_status.last_exit_code, Some(37));
            assert_eq!(final_status.last_exit_signal, None);
            assert_eq!(final_status.pid, None);
            assert_eq!(final_status.restarts, 0);
            assert_eq!(backoff_seen, cancel_backoff);
            let logs = sandbox.logs();
            assert!(logs.contains("Started: pid="));
            assert_eq!(logs.contains("Exited: code=37"), headroom >= 200);
            assert_eq!(logs.contains("Restart scheduled:"), cancel_backoff);
            let stored: RuntimeStatus =
                serde_json::from_slice(&fs::read(sandbox.paths.status("api")).unwrap()).unwrap();
            assert_eq!(stored.state, ServiceState::Failed);
            assert_eq!(stored.reason, final_status.reason);
            assert_eq!(stored.last_exit_code, Some(37));
        }
    })
    .await
    .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn batch_pause_masks_failures_unless_the_exit_code_is_saved() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for preserve_exit in [false, true] {
            let sandbox = RunnerSandbox::new();
            let batch = sandbox.temp.path().join("start.cmd");
            let ending = if preserve_exit {
                "set \"SERVICE_EXIT_CODE=%ERRORLEVEL%\"\r\nif errorlevel 1 pause\r\nexit /b %SERVICE_EXIT_CODE%\r\n"
            } else {
                "if errorlevel 1 pause\r\n"
            };
            fs::write(
                &batch,
                format!("@echo off\r\n\"%SystemRoot%\\System32\\cmd.exe\" /d /c exit 37\r\n{ending}"),
            )
            .unwrap();
            let mut config = sandbox.config(0, RestartPolicy::OnFailure);
            config.command = vec![batch.to_string_lossy().into_owned()];
            config.resolved_executable = fs::canonicalize(batch).unwrap();
            let mut runner = sandbox.runner(&config);
            let mut control = runner.stdin.take().unwrap();
            let mut reader = BufReader::new(runner.stdout.take().unwrap());
            let status = loop {
                let status: RuntimeStatus = ipc::read_frame(&mut reader).await.unwrap().unwrap();
                if matches!(status.state, ServiceState::Stopped | ServiceState::Backoff) {
                    break status;
                }
            };
            if preserve_exit {
                assert_eq!(status.state, ServiceState::Backoff);
                assert_eq!(status.last_exit_code, Some(37));
                ipc::write_frame(&mut control, &Control::Stop)
                    .await
                    .unwrap();
            } else {
                assert_eq!(status.state, ServiceState::Stopped);
                assert_eq!(status.last_exit_code, Some(0));
            }
            assert!(runner.wait().await.unwrap().success());
            assert_eq!(sandbox.logs().contains("Restart scheduled:"), preserve_exit);
        }
    })
    .await
    .unwrap();
}

#[cfg(windows)]
#[tokio::test]
async fn long_running_batch_remains_running_past_control_timeouts() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let sandbox = RunnerSandbox::new();
        let gate = sandbox.temp.path().join("finish");
        let batch = sandbox.temp.path().join("long start.cmd");
        let mut config = sandbox.config(0, RestartPolicy::OnFailure);
        fs::write(
            &batch,
            format!(
                "@echo off\r\n\"{}\" --exact fixture --nocapture\r\nset \"SERVICE_EXIT_CODE=%ERRORLEVEL%\"\r\nexit /b %SERVICE_EXIT_CODE%\r\n",
                config.resolved_executable.display()
            ),
        )
        .unwrap();
        config.environment.insert(
            "SVCNEST_DIAGNOSTIC_GATE".into(),
            gate.to_string_lossy().into_owned(),
        );
        config.command = vec![batch.to_string_lossy().into_owned()];
        config.resolved_executable = fs::canonicalize(batch).unwrap();
        let mut runner = sandbox.runner(&config);
        let _control = runner.stdin.take().unwrap();
        let mut reader = BufReader::new(runner.stdout.take().unwrap());
        loop {
            let status: RuntimeStatus = ipc::read_frame(&mut reader).await.unwrap().unwrap();
            if status.state == ServiceState::Running {
                break;
            }
        }
        // stop_timeout と daemon の起動待ち（stop_timeout + 15 秒）を越えても実行を続ける。
        tokio::time::sleep(Duration::from_secs(16)).await;
        assert!(runner.try_wait().unwrap().is_none());
        let status: RuntimeStatus =
            serde_json::from_slice(&fs::read(sandbox.paths.status("api")).unwrap()).unwrap();
        assert_eq!(status.state, ServiceState::Running);
        assert!(status.pid.is_some());
        assert!(status.last_exit_code.is_none());
        assert!(!sandbox.logs().contains("Exited:"));
        fs::write(gate, b"finish").unwrap();
        assert!(runner.wait().await.unwrap().success());
        let status: RuntimeStatus =
            serde_json::from_slice(&fs::read(sandbox.paths.status("api")).unwrap()).unwrap();
        assert_eq!(status.state, ServiceState::Stopped);
        assert_eq!(status.last_exit_code, Some(0));
        assert!(sandbox.logs().contains("stopped: cause=exit-success restart=on-failure"));
    })
    .await
    .unwrap();
}
