use crate::{
    config::{self, ServiceConfig},
    error::fail,
    ipc::{
        self, Action, Command, Listener, PROTOCOL_VERSION, Request, Response, RuntimeStatus,
        ServiceState, ServiceStatus, Snapshot, Target,
    },
    logging,
    paths::{Lock, Paths},
    process, resolve,
    runner::{self, Control},
};
use anyhow::{Context, Result};
use std::{collections::BTreeMap, fs::OpenOptions, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::BufReader,
    process::{ChildStdin, Command as ProcessCommand},
    sync::{Mutex, watch},
    task::JoinSet,
};

struct RunnerHandle {
    control: Option<ChildStdin>,
    status: watch::Receiver<RuntimeStatus>,
    done: watch::Receiver<bool>,
    stop_timeout: Duration,
}

impl RunnerHandle {
    async fn spawn(paths: &Paths, config: &ServiceConfig) -> Result<Self> {
        let stderr = OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.logs.join("daemon.log"))?;
        let mut command = ProcessCommand::new(std::env::current_exe()?);
        command
            .arg("--home")
            .arg(&paths.home)
            .arg("__runner")
            .arg(&config.name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        // launchd の daemon グループ終了処理から runner を分離し、EOF による回収を完了させる。
        #[cfg(unix)]
        command.process_group(0);
        // console を持たない daemon から起動しても、runner 用の画面を作らない。
        // 対象プロセスはこの非表示 console を継承し、CTRL_BREAK で停止できる。
        #[cfg(windows)]
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
        let mut child = command.spawn()?;
        let control = child.stdin.take().context("Runner control pipe missing")?;
        let output = child.stdout.take().context("Runner event pipe missing")?;
        let initial = RuntimeStatus {
            state: ServiceState::Starting,
            ..Default::default()
        };
        let (status_tx, status) = watch::channel(initial);
        let (done_tx, done) = watch::channel(false);
        let paths = paths.clone();
        let name = config.name.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(output);
            let read = async {
                loop {
                    match ipc::read_frame::<RuntimeStatus>(&mut reader).await {
                        Ok(Some(status)) => {
                            status_tx.send_replace(status);
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
            };
            let (_, result) = tokio::join!(read, child.wait());
            let mut final_status = status_tx.borrow().clone();
            if !matches!(
                final_status.state,
                ServiceState::Stopped | ServiceState::Failed
            ) {
                final_status.state = ServiceState::Failed;
                final_status.pid = None;
                final_status.started_at = None;
                final_status.reason = Some("runner-exit".to_owned());
                if let Ok(exit) = result {
                    let (code, signal) = process::exit_details(exit);
                    final_status.last_exit_code = code;
                    final_status.last_exit_signal = signal;
                }
                let _ = crate::paths::atomic_write(
                    &paths.status(&name),
                    &serde_json::to_vec(&final_status).unwrap_or_default(),
                );
                status_tx.send_replace(final_status);
            }
            done_tx.send_replace(true);
        });
        Ok(Self {
            control: Some(control),
            status,
            done,
            stop_timeout: Duration::from_millis(config.stop_timeout_ms),
        })
    }

    fn alive(&self) -> bool {
        !*self.done.borrow()
    }

    async fn wait_started(&mut self) -> Result<()> {
        tokio::time::timeout(self.stop_timeout + Duration::from_secs(15), async {
            loop {
                let status = self.status.borrow().clone();
                match status.state {
                    ServiceState::Starting => (),
                    ServiceState::Failed if status.reason.as_deref() == Some("service-in-use") => return fail("SERVICE_IN_USE", "The previous runner or a foreground instance still owns this service"),
                    ServiceState::Failed if status.reason.as_deref() == Some("spawn-error") => return fail("SPAWN_FAILED", "Target could not start; check svcnest logs and doctor"),
                    ServiceState::Failed if status.reason.as_deref() == Some("runner-exit") => return fail("RUNNER_FAILED", "Runner exited before starting the service; check daemon.log and doctor"),
                    _ => return Ok(()),
                }
                self.status.changed().await.context("Runner exited without a startup result")?;
            }
        }).await.context("Runner startup timed out")?
    }

    async fn stop(&mut self) -> Result<()> {
        if let Some(mut control) = self.control.take() {
            let _ = tokio::time::timeout(
                Duration::from_secs(2),
                ipc::write_frame(&mut control, &Control::Stop),
            )
            .await;
            // 書き込みに失敗しても EOF を確実に通知する。
            drop(control);
        }
        tokio::time::timeout(self.stop_timeout + Duration::from_secs(12), async {
            while !*self.done.borrow() {
                self.done
                    .changed()
                    .await
                    .context("Runner completion channel closed")?;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await
        .context("Runner did not finish stopping; restart was refused")??;
        Ok(())
    }
}

struct State {
    paths: Paths,
    runners: BTreeMap<String, RunnerHandle>,
    shutting_down: bool,
}

impl State {
    fn targets(&self, target: &Target, operation: &str) -> Result<Vec<ServiceConfig>> {
        let configs = if let Some(name) = &target.name {
            vec![config::load_named(&self.paths, name)?]
        } else {
            config::all(&self.paths)?
        };
        resolve::service::resolve(
            &configs,
            target.name.as_deref(),
            &target.cwd,
            target.all,
            operation,
        )
    }

    fn snapshot(&self, configs: &[ServiceConfig]) -> Result<Snapshot> {
        let mut services = Vec::new();
        for config in configs {
            let runtime = if let Some(handle) = self
                .runners
                .get(&config.name)
                .filter(|handle| handle.alive())
            {
                handle.status.borrow().clone()
            } else if Lock::try_acquire(&self.paths.service_lock(&config.name))?.is_none() {
                let mut status: RuntimeStatus = std::fs::read(self.paths.status(&config.name))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default();
                if status.state != ServiceState::Foreground {
                    status.state = ServiceState::Stopping;
                    status.reason = Some("previous-runner-stopping".to_owned());
                }
                status
            } else {
                runner::stored_status(&self.paths, config)
            };
            services.push(ServiceStatus::new(config, runtime));
        }
        Ok(Snapshot {
            schema_version: 1,
            services,
        })
    }

    async fn start(&mut self, config: &ServiceConfig) -> Result<()> {
        if self
            .runners
            .get(&config.name)
            .is_some_and(RunnerHandle::alive)
        {
            return Ok(());
        }
        if Lock::try_acquire(&self.paths.service_lock(&config.name))?.is_none() {
            let status = std::fs::read(self.paths.status(&config.name))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<RuntimeStatus>(&bytes).ok());
            if status.is_some_and(|status| status.state == ServiceState::Foreground) {
                return fail(
                    "SERVICE_RUNNING",
                    "Stop the foreground instance before starting the service",
                );
            }
        }
        let mut handle = RunnerHandle::spawn(&self.paths, config).await?;
        let result = handle.wait_started().await;
        if result.is_err() {
            let _ = handle.stop().await;
        }
        self.runners.insert(config.name.clone(), handle);
        result
    }

    async fn stop_many(&mut self, names: &[String]) -> Result<()> {
        for name in names {
            if !self.runners.get(name).is_some_and(RunnerHandle::alive)
                && Lock::try_acquire(&self.paths.service_lock(name))?.is_none()
            {
                return fail(
                    "SERVICE_IN_USE",
                    format!("'{name}' is owned by another runner or a foreground instance"),
                );
            }
        }
        let mut tasks = JoinSet::new();
        for name in names {
            if let Some(mut handle) = self.runners.remove(name) {
                let name = name.clone();
                tasks.spawn(async move {
                    let result = handle.stop().await;
                    (name, handle, result)
                });
            }
        }
        let mut first_error = None;
        while let Some(result) = tasks.join_next().await {
            let (name, handle, result) = result?;
            self.runners.insert(name, handle);
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        self.shutting_down = true;
        let names = self
            .runners
            .iter()
            .filter(|(_, handle)| handle.alive())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        self.stop_many(&names).await
    }

    async fn handle(&mut self, command: Command) -> Result<Snapshot> {
        if self.shutting_down {
            return fail("DAEMON_STOPPING", "Daemon is shutting down");
        }
        match command {
            Command::Ping => Ok(Snapshot {
                schema_version: 1,
                services: Vec::new(),
            }),
            Command::Inspect { target } => {
                let configs = match target {
                    Some(target) => self.targets(&target, "status")?,
                    None => config::all(&self.paths)?,
                };
                self.snapshot(&configs)
            }
            Command::Add { config, replace } => {
                config.validate()?;
                if self.paths.config(&config.name).exists() && !replace {
                    return fail(
                        "SERVICE_EXISTS",
                        format!("Service '{}' already exists; use --replace", config.name),
                    );
                }
                if self
                    .runners
                    .get(&config.name)
                    .is_some_and(RunnerHandle::alive)
                {
                    return fail(
                        "SERVICE_RUNNING",
                        "Stop the service before replacing its config",
                    );
                }
                let Some(_lock) = Lock::try_acquire(&self.paths.service_lock(&config.name))? else {
                    return fail(
                        "SERVICE_IN_USE",
                        "Service is owned by another runner or foreground instance",
                    );
                };
                config::save(&self.paths, &config)?;
                self.runners.remove(&config.name);
                logging::remove_if_exists(&self.paths.status(&config.name))?;
                drop(_lock);
                self.snapshot(&[*config])
            }
            Command::Action {
                action,
                target,
                now,
            } => {
                let mut configs = self.targets(&target, &action.to_string())?;
                let names = configs.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
                match action {
                    Action::Stop => self.stop_many(&names).await?,
                    Action::Restart => {
                        self.stop_many(&names).await?;
                        for config in &configs {
                            self.start(config).await?;
                        }
                    }
                    Action::Start => {
                        for config in &configs {
                            self.start(config).await?;
                        }
                    }
                    Action::Enable | Action::Disable => {
                        for config in &mut configs {
                            config.enabled = matches!(action, Action::Enable);
                            config::save(&self.paths, config)?;
                        }
                        if now {
                            if matches!(action, Action::Enable) {
                                for config in &configs {
                                    self.start(config).await?;
                                }
                            } else {
                                self.stop_many(&names).await?;
                            }
                        }
                    }
                }
                self.snapshot(&configs)
            }
            Command::Remove {
                target,
                stop,
                purge,
            } => {
                let configs = self.targets(&target, "remove")?;
                let names = configs.iter().map(|c| c.name.clone()).collect::<Vec<_>>();
                if stop {
                    self.stop_many(&names).await?;
                }
                // --all の途中まで消さないよう、全件を削除前に検査する。
                let mut locks = Vec::new();
                for config in &configs {
                    if self
                        .runners
                        .get(&config.name)
                        .is_some_and(RunnerHandle::alive)
                    {
                        return fail(
                            "SERVICE_RUNNING",
                            format!("'{}' is running; use remove --stop", config.name),
                        );
                    }
                    let Some(lock) = Lock::try_acquire(&self.paths.service_lock(&config.name))?
                    else {
                        return fail("SERVICE_IN_USE", "Service is still in use");
                    };
                    locks.push(lock);
                }
                for config in configs {
                    std::fs::remove_file(self.paths.config(&config.name))?;
                    self.runners.remove(&config.name);
                    logging::remove_if_exists(&self.paths.status(&config.name))?;
                    if purge {
                        logging::purge(&self.paths, &config.name)?;
                    }
                }
                Ok(Snapshot {
                    schema_version: 1,
                    services: Vec::new(),
                })
            }
            Command::Shutdown => {
                self.shutdown().await?;
                Ok(Snapshot {
                    schema_version: 1,
                    services: Vec::new(),
                })
            }
        }
    }
}

pub async fn serve(paths: Paths) -> Result<()> {
    let Some(_lock) = Lock::try_acquire(&paths.daemon_lock())? else {
        return Ok(());
    };
    let mut listener = Listener::bind(&paths)?;
    crate::paths::atomic_write(
        &paths.runtime.join("daemon.pid"),
        std::process::id().to_string().as_bytes(),
    )?;
    let state = Arc::new(Mutex::new(State {
        paths: paths.clone(),
        runners: BTreeMap::new(),
        shutting_down: false,
    }));
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let boot_state = state.clone();
    tokio::spawn(async move {
        let mut state = boot_state.lock().await;
        if let Ok(files) = config::config_files(&state.paths) {
            for path in files {
                match config::load(&path) {
                    Ok(config) if config.enabled => {
                        if let Err(error) = state.start(&config).await {
                            eprintln!("Cannot autostart {}: {error:#}", config.name);
                        }
                    }
                    Ok(_) => (),
                    Err(error) => eprintln!("{error:#}"),
                }
            }
        }
    });
    let interrupt = process::interrupt();
    tokio::pin!(interrupt);
    let mut clients = JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut interrupt => break,
            _ = shutdown_rx.changed() => if *shutdown_rx.borrow() { break; },
            result = listener.accept() => match result {
                Ok(stream) => {
                    let state = state.clone(); let shutdown_tx = shutdown_tx.clone();
                    clients.spawn(async move {
                        let mut reader = BufReader::new(stream);
                        let request = tokio::time::timeout(Duration::from_secs(5), ipc::read_frame::<Request>(&mut reader)).await;
                        let Ok(Ok(Some(request))) = request else { return; };
                        let shutdown = matches!(request.command, Command::Shutdown);
                        let result = if request.version != PROTOCOL_VERSION { fail("PROTOCOL_VERSION", "Incompatible IPC protocol version") }
                            else if matches!(request.command, Command::Ping) { Ok(Snapshot { schema_version: 1, services: Vec::new() }) }
                            else { state.lock().await.handle(request.command).await };
                        let successful = result.is_ok();
                        let _ = ipc::write_frame(reader.get_mut(), &Response::from_result(result)).await;
                        if shutdown && successful { shutdown_tx.send_replace(true); }
                    });
                }
                Err(error) => eprintln!("IPC accept failed: {error:#}"),
            },
            Some(_) = clients.join_next(), if !clients.is_empty() => (),
        }
    }
    let result = state.lock().await.shutdown().await;
    clients.abort_all();
    drop(listener);
    #[cfg(unix)]
    logging::remove_if_exists(&paths.endpoint())?;
    logging::remove_if_exists(&paths.runtime.join("daemon.pid"))?;
    result
}

pub async fn ensure(paths: &Paths) -> Result<()> {
    if ipc::ping(paths).await {
        return Ok(());
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(paths.logs.join("daemon.log"))?;
    let executable = std::env::current_exe()?;
    #[cfg(any(unix, windows))]
    let executable = crate::runtime::prepare(paths, &executable)?;
    let mut command = ProcessCommand::new(executable);
    command
        .arg("--home")
        .arg(&paths.home)
        // 親で公開済みの内容ハッシュを確認しているため、子側の再ハッシュを省く。
        .args(["daemon", "serve", "--prepared-runtime"])
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(
        windows_sys::Win32::System::Threading::DETACHED_PROCESS
            | windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP,
    );
    let mut child = command.spawn().context("Cannot launch svcnest daemon")?;
    for _ in 0..200 {
        if ipc::ping(paths).await {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            // 同時に起動した別 CLI が先に daemon ロックを取得した場合は接続を待つ。
            if !status.success() {
                return fail(
                    "DAEMON_START_FAILED",
                    format!(
                        "Daemon exited with {status}; inspect {}",
                        paths.logs.join("daemon.log").display()
                    ),
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    fail(
        "DAEMON_START_FAILED",
        format!(
            "Cannot connect to daemon; inspect {}",
            paths.logs.join("daemon.log").display()
        ),
    )
}

pub async fn offline_snapshot(paths: &Paths, target: Option<&Target>) -> Result<Snapshot> {
    let configs = match target {
        Some(target) => {
            let configs = if let Some(name) = &target.name {
                vec![config::load_named(paths, name)?]
            } else {
                config::all(paths)?
            };
            resolve::service::resolve(
                &configs,
                target.name.as_deref(),
                &target.cwd,
                target.all,
                "status",
            )?
        }
        None => config::all(paths)?,
    };
    let mut services = Vec::new();
    for config in &configs {
        let mut runtime = runner::stored_status(paths, config);
        if Lock::try_acquire(&paths.service_lock(&config.name))?.is_none() {
            runtime.state = ServiceState::Unavailable;
            runtime.reason = Some("daemon-unavailable-or-foreground".to_owned());
        }
        services.push(ServiceStatus::new(config, runtime));
    }
    Ok(Snapshot {
        schema_version: 1,
        services,
    })
}
