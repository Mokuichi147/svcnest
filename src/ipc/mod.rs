use crate::{
    config::{RestartPolicy, ServiceConfig},
    error::{ServiceError, fail},
    paths::Paths,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{path::PathBuf, time::Duration};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceState {
    Starting,
    Running,
    Stopping,
    #[default]
    Stopped,
    Backoff,
    Failed,
    Foreground,
    Unavailable,
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Backoff => "backoff",
            Self::Failed => "failed",
            Self::Foreground => "foreground",
            Self::Unavailable => "unavailable",
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RuntimeStatus {
    pub state: ServiceState,
    pub pid: Option<u32>,
    pub started_at: Option<DateTime<Utc>>,
    pub restarts: u32,
    pub last_exit_code: Option<i32>,
    pub last_exit_signal: Option<i32>,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub name: String,
    pub description: String,
    pub command: Vec<String>,
    pub resolved_executable: PathBuf,
    pub cwd: PathBuf,
    pub enabled: bool,
    pub restart: RestartPolicy,
    pub uptime_seconds: Option<u64>,
    #[serde(flatten)]
    pub runtime: RuntimeStatus,
}

impl ServiceStatus {
    pub fn new(config: &ServiceConfig, runtime: RuntimeStatus) -> Self {
        let uptime_seconds = runtime
            .started_at
            .filter(|_| {
                matches!(
                    runtime.state,
                    ServiceState::Running | ServiceState::Stopping | ServiceState::Foreground
                )
            })
            .map(|started| (Utc::now() - started).num_seconds().max(0) as u64);
        Self {
            name: config.name.clone(),
            description: config.description.clone(),
            command: config.command.clone(),
            resolved_executable: config.resolved_executable.clone(),
            cwd: config.cwd.clone(),
            enabled: config.enabled,
            restart: config.restart,
            uptime_seconds,
            runtime,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub services: Vec<ServiceStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Target {
    pub name: Option<String>,
    pub cwd: PathBuf,
    pub all: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Start,
    Stop,
    Restart,
    Enable,
    Disable,
}

impl std::fmt::Display for Action {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Enable => "enable",
            Self::Disable => "disable",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Command {
    Ping,
    Add {
        config: ServiceConfig,
        replace: bool,
    },
    Action {
        action: Action,
        target: Target,
        now: bool,
    },
    Inspect {
        target: Option<Target>,
    },
    Remove {
        target: Target,
        stop: bool,
        purge: bool,
    },
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub command: Command,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub version: u32,
    pub result: Option<Snapshot>,
    pub error: Option<RpcError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl Response {
    pub fn from_result(result: Result<Snapshot>) -> Self {
        match result {
            Ok(result) => Self {
                version: PROTOCOL_VERSION,
                result: Some(result),
                error: None,
            },
            Err(error) => Self {
                version: PROTOCOL_VERSION,
                result: None,
                error: Some(RpcError {
                    code: ServiceError::code(&error).to_owned(),
                    message: format!("{error:#}"),
                }),
            },
        }
    }
    pub fn into_result(self) -> Result<Snapshot> {
        if self.version != PROTOCOL_VERSION {
            return fail(
                "PROTOCOL_VERSION",
                "Incompatible daemon protocol; restart the daemon with this svcnest version",
            );
        }
        if let Some(error) = self.error {
            return Err(ServiceError::new(error.code, error.message).into());
        }
        self.result.context("Daemon returned an empty response")
    }
}

pub trait Connection: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Connection for T {}
pub type Stream = Box<dyn Connection>;

pub async fn read_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Option<T>> {
    let mut frame = Vec::new();
    loop {
        let bytes = reader.fill_buf().await?;
        if bytes.is_empty() {
            if frame.is_empty() {
                return Ok(None);
            }
            return fail(
                "INVALID_FRAME",
                "IPC connection ended in the middle of a frame",
            );
        }
        let count = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |p| p + 1);
        if frame.len() + count > MAX_FRAME {
            return fail("FRAME_TOO_LARGE", "IPC message exceeds 1 MiB");
        }
        frame.extend_from_slice(&bytes[..count]);
        let complete = bytes[count - 1] == b'\n';
        reader.consume(count);
        if complete {
            break;
        }
    }
    serde_json::from_slice(&frame)
        .map(Some)
        .map_err(|_| ServiceError::new("INVALID_FRAME", "Invalid IPC JSON").into())
}

pub async fn write_frame<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    if bytes.len() >= MAX_FRAME {
        return fail("FRAME_TOO_LARGE", "IPC message exceeds 1 MiB");
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

pub async fn request(paths: &Paths, command: Command) -> Result<Snapshot> {
    // 大量の --all 停止にも対応する。各 runner の停止自体には別の期限がある。
    tokio::time::timeout(Duration::from_secs(660), async {
        let mut stream = connect(paths).await?;
        write_frame(
            &mut stream,
            &Request {
                version: PROTOCOL_VERSION,
                command,
            },
        )
        .await?;
        let response: Response = read_frame(&mut BufReader::new(stream))
            .await?
            .context("Daemon disconnected before replying")?;
        response.into_result()
    })
    .await
    .context("Daemon request timed out")?
}

pub async fn ping(paths: &Paths) -> bool {
    tokio::time::timeout(Duration::from_secs(2), request(paths, Command::Ping))
        .await
        .is_ok_and(|result| result.is_ok())
}

#[cfg(unix)]
pub struct Listener {
    inner: tokio::net::UnixListener,
}

#[cfg(unix)]
impl Listener {
    pub fn bind(paths: &Paths) -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let endpoint = paths.endpoint();
        match std::fs::remove_file(&endpoint) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let inner = tokio::net::UnixListener::bind(&endpoint)?;
        std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self { inner })
    }
    pub async fn accept(&mut self) -> Result<Stream> {
        let (stream, _) = self.inner.accept().await?;
        if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
            return fail("IPC_ACCESS_DENIED", "IPC peer is not the current user");
        }
        Ok(Box::new(stream))
    }
}

#[cfg(unix)]
async fn connect(paths: &Paths) -> Result<Stream> {
    let stream = tokio::net::UnixStream::connect(paths.endpoint()).await?;
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return fail("IPC_ACCESS_DENIED", "IPC peer is not the current user");
    }
    Ok(Box::new(stream))
}

#[cfg(windows)]
pub struct Listener {
    server: Option<tokio::net::windows::named_pipe::NamedPipeServer>,
    name: String,
}

#[cfg(windows)]
impl Listener {
    pub fn bind(paths: &Paths) -> Result<Self> {
        let name = paths.endpoint();
        Ok(Self {
            server: Some(crate::platform::windows::private_pipe(&name, true)?),
            name,
        })
    }
    pub async fn accept(&mut self) -> Result<Stream> {
        let server = self.server.as_ref().context("Named pipe listener closed")?;
        server.connect().await?;
        let next = crate::platform::windows::private_pipe(&self.name, false)?;
        let connected = self
            .server
            .replace(next)
            .context("Named pipe listener closed")?;
        crate::platform::windows::verify_pipe_peer(&connected, false)?;
        Ok(Box::new(connected))
    }
}

#[cfg(windows)]
async fn connect(paths: &Paths) -> Result<Stream> {
    use tokio::net::windows::named_pipe::ClientOptions;
    for _ in 0..40 {
        match ClientOptions::new().open(paths.endpoint()) {
            Ok(client) => {
                // パイプ名を別ユーザーが先取りしても、設定や環境値を送る前に所有者を検証する。
                crate::platform::windows::verify_pipe_peer(&client, true)?;
                return Ok(Box::new(client));
            }
            Err(error)
                if error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_PIPE_BUSY as i32) =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await
            }
            Err(error) => return Err(error.into()),
        }
    }
    fail("IPC_BUSY", "Named pipe remained busy")
}
