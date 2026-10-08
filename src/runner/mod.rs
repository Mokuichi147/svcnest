pub mod policy;

use crate::{
    config::{self, ServiceConfig},
    ipc::{self, RuntimeStatus, ServiceState},
    logging::{self, RotatingLog},
    paths::{Lock, Paths, atomic_write},
    process::{self, ProcessTree},
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::BufReader, sync::mpsc};

#[derive(Deserialize, Serialize)]
pub enum Control {
    Stop,
}

pub async fn serve(paths: &Paths, name: &str) -> Result<()> {
    config::validate_name(name)?;
    process::prepare_supervisor()?;
    let interrupt = process::interrupt();
    tokio::pin!(interrupt);
    let config = config::load_named(paths, name)?;
    let timeout = Duration::from_millis(config.stop_timeout_ms);
    let (stop_tx, mut stop_rx) = mpsc::channel(1);
    // EOF、壊れた制御フレーム、Stop はいずれも安全な停止として扱う。
    tokio::spawn(async move {
        let mut reader = BufReader::new(tokio::io::stdin());
        let _ = ipc::read_frame::<Control>(&mut reader).await;
        let _ = stop_tx.send(()).await;
    });
    let mut output = tokio::io::stdout();
    let mut status = RuntimeStatus {
        state: ServiceState::Starting,
        ..Default::default()
    };
    emit(paths, name, &status, &mut output).await?;
    // daemon が落ちた直後は、旧 runner がツリーを回収してロックを手放すまで待つ。
    let deadline = Instant::now() + timeout + Duration::from_secs(8);
    let _lock = loop {
        if let Some(lock) = Lock::try_acquire(&paths.service_lock(name))? {
            break lock;
        }
        if Instant::now() >= deadline {
            status.state = ServiceState::Failed;
            status.reason = Some("service-in-use".to_owned());
            emit(paths, name, &status, &mut output).await?;
            return Ok(());
        }
        tokio::select! {
            _ = stop_rx.recv() => return Ok(()),
            _ = &mut interrupt => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(50)) => (),
        }
    };
    if stop_rx.try_recv().is_ok() {
        return Ok(());
    }
    let log = Arc::new(Mutex::new(RotatingLog::new(&paths.log(name))?));
    let mut backoff = policy::Backoff::default();
    loop {
        let mut tree = match ProcessTree::spawn(&config, false) {
            Ok(tree) => tree,
            Err(error) => {
                log.lock()
                    .map_err(|_| anyhow::anyhow!("Log writer lock poisoned"))?
                    .record("svcnest", format!("Spawn failed: {error:#}").as_bytes())?;
                status.state = ServiceState::Failed;
                status.reason = Some("spawn-error".to_owned());
                emit(paths, name, &status, &mut output).await?;
                return Ok(());
            }
        };
        // env-file の読み取りなど、spawn 前の時間を安定稼働に含めない。
        let started = Instant::now();
        let stdout = tree
            .child
            .stdout
            .take()
            .context("Target stdout pipe missing")?;
        let stderr = tree
            .child
            .stderr
            .take()
            .context("Target stderr pipe missing")?;
        let mut stdout_task = tokio::spawn(logging::capture(stdout, "stdout", log.clone()));
        let mut stderr_task = tokio::spawn(logging::capture(stderr, "stderr", log.clone()));
        let mut stdout_done = None;
        let mut stderr_done = None;
        let mut log_failed = false;
        status.state = ServiceState::Running;
        status.pid = tree.child.id();
        status.started_at = Some(Utc::now());
        status.reason = None;
        // 出力先が切れた場合も target を停止してから runner を終了する。
        let mut manual = emit(paths, name, &status, &mut output).await.is_err();
        let mut scan = tokio::time::interval(Duration::from_millis(150));
        let exit = if manual {
            None
        } else {
            loop {
                tokio::select! {
                    _ = stop_rx.recv() => { manual = true; break None; }
                    _ = &mut interrupt => { manual = true; break None; }
                    result = tree.child.wait() => break Some(result?),
                    result = &mut stdout_task, if stdout_done.is_none() => {
                        let result = result.map_err(anyhow::Error::from).and_then(|result| result);
                        log_failed = result.is_err(); stdout_done = Some(result);
                        if log_failed { break None; }
                    }
                    result = &mut stderr_task, if stderr_done.is_none() => {
                        let result = result.map_err(anyhow::Error::from).and_then(|result| result);
                        log_failed = result.is_err(); stderr_done = Some(result);
                        if log_failed { break None; }
                    }
                    _ = scan.tick() => { tree.refresh()?; }
                }
            }
        };
        if manual || log_failed {
            status.state = ServiceState::Stopping;
            let _ = emit(paths, name, &status, &mut output).await;
        }
        // 親が自然終了したときも子孫を完全に回収してから policy を判断する。
        let uptime = started.elapsed();
        let stopped = tree.stop(timeout, false).await?;
        let exit = exit.unwrap_or(stopped);
        if stdout_done.is_none() {
            stdout_done = Some(
                stdout_task
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result),
            );
        }
        if stderr_done.is_none() {
            stderr_done = Some(
                stderr_task
                    .await
                    .map_err(anyhow::Error::from)
                    .and_then(|result| result),
            );
        }
        log_failed |= stdout_done.is_some_and(|result| result.is_err())
            || stderr_done.is_some_and(|result| result.is_err());
        let (code, signal) = process::exit_details(exit);
        status.last_exit_code = code;
        status.last_exit_signal = signal;
        status.pid = None;
        status.started_at = None;
        if log_failed {
            status.state = ServiceState::Failed;
            status.reason = Some("log-error".to_owned());
            let _ = emit(paths, name, &status, &mut output).await;
            return Ok(());
        }
        if manual || !policy::should_restart(config.restart, exit.success(), false) {
            status.state = if !manual && !exit.success() {
                ServiceState::Failed
            } else {
                ServiceState::Stopped
            };
            status.reason = if manual {
                None
            } else if !exit.success() {
                Some("exit-failure".to_owned())
            } else {
                None
            };
            let _ = emit(paths, name, &status, &mut output).await;
            return Ok(());
        }
        let Some(delay) = backoff.next(Instant::now(), uptime) else {
            status.state = ServiceState::Failed;
            status.reason = Some("restart-limit".to_owned());
            let _ = emit(paths, name, &status, &mut output).await;
            return Ok(());
        };
        status.state = ServiceState::Backoff;
        status.reason = Some("restart-backoff".to_owned());
        if emit(paths, name, &status, &mut output).await.is_err() {
            return Ok(());
        }
        tokio::select! {
            _ = stop_rx.recv() => {
                status.state = ServiceState::Stopped;
                status.reason = None;
                let _ = emit(paths, name, &status, &mut output).await;
                return Ok(());
            }
            _ = &mut interrupt => return Ok(()),
            _ = tokio::time::sleep(delay) => (),
        }
        status.restarts = status.restarts.saturating_add(1);
    }
}

async fn emit(
    paths: &Paths,
    name: &str,
    status: &RuntimeStatus,
    output: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<()> {
    atomic_write(&paths.status(name), &serde_json::to_vec(status)?)?;
    ipc::write_frame(output, status).await
}

pub fn stored_status(paths: &Paths, config: &ServiceConfig) -> RuntimeStatus {
    let status: Option<RuntimeStatus> = std::fs::read(paths.status(&config.name))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    match status {
        Some(status) if matches!(status.state, ServiceState::Stopped | ServiceState::Failed) => {
            status
        }
        _ => RuntimeStatus::default(),
    }
}
