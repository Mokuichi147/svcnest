use crate::{
    config::{self, RestartPolicy, ServiceConfig},
    daemon,
    error::fail,
    ipc::{self, Action, Command, RuntimeStatus, ServiceState, Snapshot, Target},
    logging,
    paths::{Lock, Paths, atomic_write, readable_path},
    platform,
    process::{self, ProcessTree},
    resolve,
};
use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(
    name = "svcnest",
    version,
    about = "Manage local programs from their project directory"
)]
pub struct Cli {
    #[arg(
        long,
        global = true,
        value_name = "DIRECTORY",
        help = "Override the per-user storage directory (also SVCNEST_HOME)"
    )]
    pub home: Option<PathBuf>,
    #[command(subcommand)]
    pub command: CliCommand,
}

#[derive(Subcommand)]
pub enum CliCommand {
    #[command(about = "Register a command and capture the working directory and PATH")]
    Add(AddArgs),
    #[command(about = "Start a service in the background")]
    Start(TargetArgs),
    Stop(TargetArgs),
    Restart(TargetArgs),
    Status(StatusArgs),
    #[command(about = "List every registered service")]
    List {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Run a stopped service in the foreground without restart policy")]
    Run(SingleTarget),
    Logs(LogsArgs),
    #[command(about = "Start a service automatically when the daemon starts")]
    Enable(ToggleArgs),
    Disable(ToggleArgs),
    Remove(RemoveArgs),
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    #[command(name = "__runner", hide = true)]
    Runner {
        name: String,
    },
}

#[derive(Args)]
pub struct AddArgs {
    pub name: String,
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    #[arg(
        long,
        value_name = "EXECUTABLE",
        help = "Select the PowerShell executable for a Windows .ps1 script"
    )]
    pub shell: Option<String>,
    #[arg(long, value_enum, default_value_t = RestartPolicy::OnFailure)]
    pub restart: RestartPolicy,
    #[arg(long, value_parser = parse_env, value_name = "KEY=VALUE")]
    pub env: Vec<(String, String)>,
    #[arg(long)]
    pub env_file: Option<PathBuf>,
    #[arg(long)]
    pub enable: bool,
    #[arg(long, default_value = "")]
    pub description: String,
    #[arg(long, default_value = "10s", value_parser = parse_timeout)]
    pub stop_timeout: u64,
    #[arg(long)]
    pub replace: bool,
    #[arg(last = true, required = true, num_args = 1.., allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Args)]
pub struct TargetArgs {
    #[arg(conflicts_with = "all")]
    pub name: Option<String>,
    #[arg(long, help = "Select all services at the nearest registered directory")]
    pub all: bool,
}

impl TargetArgs {
    fn target(self) -> Result<Target> {
        // 明示した名前では cwd を使用しない。削除済みディレクトリからも操作できる。
        let cwd = if self.name.is_some() {
            PathBuf::new()
        } else {
            std::env::current_dir()?
        };
        Ok(Target {
            name: self.name,
            all: self.all,
            cwd,
        })
    }
}

#[derive(Args)]
pub struct SingleTarget {
    pub name: Option<String>,
}
#[derive(Args)]
pub struct StatusArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    #[arg(long)]
    pub json: bool,
}
#[derive(Args)]
pub struct ToggleArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    #[arg(long)]
    pub now: bool,
}
#[derive(Args)]
pub struct LogsArgs {
    pub name: Option<String>,
    #[arg(short = 'n', long = "lines", default_value_t = 100)]
    pub lines: usize,
    #[arg(short, long)]
    pub follow: bool,
}
#[derive(Args)]
pub struct RemoveArgs {
    #[command(flatten)]
    pub target: TargetArgs,
    #[arg(long)]
    pub stop: bool,
    #[arg(long)]
    pub purge: bool,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    Show {
        name: Option<String>,
        #[arg(long, help = "Show stored environment values without masking")]
        show_secrets: bool,
    },
    Path {
        name: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum DaemonCommand {
    Install {
        #[arg(long, help = "Print the OS registration without installing it")]
        dry_run: bool,
    },
    Uninstall,
    Start,
    Stop,
    Status {
        #[arg(long)]
        json: bool,
    },
    #[command(about = "Serve local IPC (used by the OS service manager)")]
    Serve {
        #[arg(long, hide = true)]
        source_executable: Option<PathBuf>,
        #[arg(long, hide = true)]
        prepared_runtime: bool,
    },
}

fn parse_env(value: &str) -> std::result::Result<(String, String), String> {
    let (key, value) = value.split_once('=').ok_or("Expected KEY=VALUE")?;
    config::validate_env_key(key).map_err(|e| e.to_string())?;
    if value.contains('\0') {
        return Err("Environment values cannot contain NUL".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

fn parse_timeout(value: &str) -> std::result::Result<u64, String> {
    let (number, scale) = if let Some(n) = value.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = value.strip_suffix('s') {
        (n, 1000)
    } else {
        (value, 1000)
    };
    let millis = number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .filter(|n| (1..=300_000).contains(n));
    millis.ok_or_else(|| "Use a timeout from 1ms to 300s, such as 10s or 500ms".to_owned())
}

pub async fn execute(cli: Cli) -> Result<i32> {
    let paths = Paths::discover(cli.home)?;
    match cli.command {
        CliCommand::Runner { name } => {
            crate::runner::serve(&paths, &name).await?;
        }
        CliCommand::Daemon { command } => return daemon_command(paths, command).await,
        CliCommand::Add(args) => {
            let config = registration_config(&args).await?;
            if paths.config(&config.name).exists() && !args.replace {
                return fail(
                    "SERVICE_EXISTS",
                    format!("Service '{}' already exists; use --replace", config.name),
                );
            }
            if args.enable {
                platform::install(&paths).await?;
            }
            daemon::ensure(&paths).await?;
            ipc::request(
                &paths,
                Command::Add {
                    config: Box::new(config),
                    replace: args.replace,
                },
            )
            .await?;
            println!("Registered {}", args.name);
        }
        CliCommand::Start(args) => action(&paths, Action::Start, args.target()?, false).await?,
        CliCommand::Stop(args) => action(&paths, Action::Stop, args.target()?, false).await?,
        CliCommand::Restart(args) => action(&paths, Action::Restart, args.target()?, false).await?,
        CliCommand::Enable(args) => {
            let target = args.target.target()?;
            local_targets(&paths, &target, "enable")?;
            platform::install(&paths).await?;
            action(&paths, Action::Enable, target, args.now).await?;
        }
        CliCommand::Disable(args) => {
            action(&paths, Action::Disable, args.target.target()?, args.now).await?
        }
        CliCommand::Status(args) => {
            let target = args.target.target()?;
            let snapshot = inspect(&paths, Some(target)).await?;
            print_snapshot(&snapshot, args.json, false)?;
        }
        CliCommand::List { json } => print_snapshot(&inspect(&paths, None).await?, json, true)?,
        CliCommand::Run(args) => return foreground(&paths, args.name).await,
        CliCommand::Logs(args) => {
            let config = single_config(&paths, args.name, "logs")?;
            logging::show(&paths.log(&config.name), args.lines, args.follow).await?;
        }
        CliCommand::Config { command } => match command {
            ConfigCommand::Show { name, show_secrets } => {
                let config = single_config(&paths, name, "config show")?;
                let mut config = if show_secrets {
                    config
                } else {
                    config.redacted()
                };
                config.cwd = readable_path(&config.cwd).into_owned();
                config.resolved_executable =
                    readable_path(&config.resolved_executable).into_owned();
                config.resolved_script = config
                    .resolved_script
                    .as_deref()
                    .map(|path| readable_path(path).into_owned());
                config.env_file = config
                    .env_file
                    .as_deref()
                    .map(|path| readable_path(path).into_owned());
                print!("{}", toml::to_string_pretty(&config)?);
            }
            ConfigCommand::Path { name } => {
                let config = single_config(&paths, name, "config path")?;
                println!("{}", readable_path(&paths.config(&config.name)).display());
            }
        },
        CliCommand::Remove(args) => {
            let target = args.target.target()?;
            let configs = local_targets(&paths, &target, "remove")?;
            daemon::ensure(&paths).await?;
            ipc::request(
                &paths,
                Command::Remove {
                    target,
                    stop: args.stop,
                    purge: args.purge,
                },
            )
            .await?;
            for config in configs {
                println!("Removed {}", config.name);
            }
        }
        CliCommand::Doctor { json } => return doctor(&paths, json).await,
    }
    Ok(0)
}

async fn registration_config(args: &AddArgs) -> Result<ServiceConfig> {
    config::validate_name(&args.name)?;
    let cwd = resolve::executable::working_directory(args.cwd.as_deref())?;
    let path = std::env::var_os("PATH");
    let executable = resolve::executable::resolve(
        &args.command[0],
        &cwd,
        path.as_deref(),
        std::env::var_os("PATHEXT").as_deref(),
    )?;
    let env_file = args
        .env_file
        .as_ref()
        .map(|path| {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                cwd.join(path)
            };
            let path = fs::canonicalize(path).context("Env-file does not exist")?;
            anyhow::ensure!(path.is_file(), "Env-file is not a file");
            Ok::<_, anyhow::Error>(path)
        })
        .transpose()?;
    let mut environment = BTreeMap::new();
    if let Some(path) = &path {
        environment.insert(
            "PATH".to_owned(),
            path.clone()
                .into_string()
                .map_err(|_| anyhow::anyhow!("PATH is not valid Unicode"))?,
        );
    }
    for (key, value) in &args.env {
        config::insert_env(&mut environment, key.clone(), value.clone());
    }
    let config = ServiceConfig {
        version: 1,
        name: args.name.clone(),
        description: args.description.clone(),
        cwd,
        command: args.command.clone(),
        resolved_executable: executable,
        resolved_script: None,
        interpreter_args: Vec::new(),
        interpreter_environment: BTreeMap::new(),
        enabled: args.enable,
        restart: args.restart,
        stop_timeout_ms: args.stop_timeout,
        env_file,
        environment,
    };
    config.validate()?;
    let environment = config.effective_environment()?;
    #[cfg(windows)]
    let config = {
        let mut config = config;
        let extension = config
            .resolved_executable
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if args.shell.is_some() && extension != "ps1" {
            return fail(
                "INVALID_SHELL",
                "--shell is only supported for Windows .ps1 scripts",
            );
        }
        if extension == "ps1" {
            let shell = resolve::windows_shell::powershell(
                &config.cwd,
                path.as_deref(),
                args.shell.as_deref(),
            )?;
            config.interpreter_environment = resolve::windows_shell::environment(&shell);
            config.resolved_script = Some(config.resolved_executable);
            config.resolved_executable = shell;
            config.interpreter_args = ["-NoLogo", "-NoProfile", "-File"]
                .into_iter()
                .map(str::to_owned)
                .collect();
        } else if matches!(extension.as_str(), "cmd" | "bat")
            && let Some((node, script)) = resolve::node_shim::resolve(
                &config.resolved_executable,
                &config.cwd,
                path.as_deref(),
                &environment,
            )
            .await?
        {
            config.resolved_executable = node;
            config.resolved_script = Some(script);
        }
        config
    };
    #[cfg(not(windows))]
    if args.shell.is_some() {
        return fail(
            "INVALID_SHELL",
            "--shell is only supported for Windows .ps1 scripts",
        );
    }
    let _ = environment;
    config.validate()?;
    Ok(config)
}

fn local_targets(paths: &Paths, target: &Target, operation: &str) -> Result<Vec<ServiceConfig>> {
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
        operation,
    )
}

fn single_config(paths: &Paths, name: Option<String>, operation: &str) -> Result<ServiceConfig> {
    let target = TargetArgs { name, all: false }.target()?;
    local_targets(paths, &target, operation)?
        .into_iter()
        .next()
        .context("Service resolution returned no service")
}

async fn action(paths: &Paths, action: Action, target: Target, now: bool) -> Result<()> {
    local_targets(paths, &target, &action.to_string())?;
    daemon::ensure(paths).await?;
    let snapshot = ipc::request(
        paths,
        Command::Action {
            action,
            target,
            now,
        },
    )
    .await?;
    for status in snapshot.services {
        println!(
            "{}: {} (enabled: {})",
            status.name,
            status.runtime.state,
            if status.enabled { "yes" } else { "no" }
        );
    }
    Ok(())
}

async fn inspect(paths: &Paths, target: Option<Target>) -> Result<Snapshot> {
    if ipc::ping(paths).await {
        ipc::request(paths, Command::Inspect { target }).await
    } else {
        daemon::offline_snapshot(paths, target.as_ref()).await
    }
}

fn print_snapshot(snapshot: &Snapshot, json: bool, table: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(snapshot)?);
        return Ok(());
    }
    if table {
        println!(
            "{:<20} {:<12} {:<10} {:<8} {:<12} CWD",
            "NAME", "STATUS", "PID", "ENABLED", "LAST EXIT"
        );
        for status in &snapshot.services {
            println!(
                "{:<20} {:<12} {:<10} {:<8} {:<12} {}",
                status.name,
                status.runtime.state.to_string(),
                status
                    .runtime
                    .pid
                    .map_or("-".to_owned(), |pid| pid.to_string()),
                if status.enabled { "yes" } else { "no" },
                last_exit(&status.runtime),
                readable_path(&status.cwd).display()
            );
        }
    } else {
        for (index, status) in snapshot.services.iter().enumerate() {
            if index > 0 {
                println!();
            }
            let uptime = status
                .uptime_seconds
                .map(|s| format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60))
                .unwrap_or_else(|| "-".into());
            println!(
                "Service:       {}\nStatus:        {}\nPID:           {}\nCommand:       {}\nExecutable:    {}\nWorking dir:   {}\nEnabled:       {}\nRestart:       {}\nUptime:        {}\nRestarts:      {}",
                status.name,
                status.runtime.state,
                status.runtime.pid.map_or("-".into(), |pid| pid.to_string()),
                display_command(&status.command),
                readable_path(&status.resolved_executable).display(),
                readable_path(&status.cwd).display(),
                if status.enabled { "yes" } else { "no" },
                status.restart,
                uptime,
                status.runtime.restarts
            );
            println!("Last exit:     {}", last_exit(&status.runtime));
            if let Some(reason) = &status.runtime.reason {
                println!("Reason:        {reason}");
            }
            if !status.description.is_empty() {
                println!("Description:   {}", status.description);
            }
        }
    }
    Ok(())
}

fn last_exit(status: &crate::ipc::RuntimeStatus) -> String {
    match (status.last_exit_code, status.last_exit_signal) {
        (_, Some(signal)) => format!("signal:{signal}"),
        (Some(code), None) => code.to_string(),
        (None, None) => "-".to_owned(),
    }
}

pub fn display_command(command: &[String]) -> String {
    // これは表示専用。実行には保存した argv と解決済みの起動方法を使用する。
    command
        .iter()
        .map(|arg| {
            if arg.is_empty()
                || arg.chars().any(|c| {
                    c.is_whitespace()
                        || matches!(c, '"' | '\'' | '\\' | '$' | ';' | '|' | '&' | '<' | '>')
                })
            {
                serde_json::to_string(arg).unwrap_or_default()
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

async fn foreground(paths: &Paths, name: Option<String>) -> Result<i32> {
    let selected = single_config(paths, name, "run")?;
    let Some(_lock) = Lock::try_acquire(&paths.service_lock(&selected.name))? else {
        return fail(
            "SERVICE_RUNNING",
            format!(
                "{} is already running under svcnest or in the foreground.\n\nStop it first:\n\n  svcnest stop {}",
                selected.name, selected.name
            ),
        );
    };
    process::prepare_supervisor()?;
    // foreground 状態を公開する前に登録し、その後の Ctrl+C を確実に受け取る。
    let signal = process::interrupt();
    tokio::pin!(signal);
    // 起動前に保存し、起動中の foreground を前の runner の停止中と誤認させない。
    let status_path = paths.status(&selected.name);
    let previous = std::fs::read(&status_path).ok();
    let mut status = RuntimeStatus {
        state: ServiceState::Foreground,
        started_at: Some(Utc::now()),
        ..Default::default()
    };
    atomic_write(&status_path, &serde_json::to_vec(&status)?)?;
    // 起動前に失敗した場合は、前回の終了状態を残す。
    let restore = |error: anyhow::Error| {
        let _ = match &previous {
            Some(bytes) => atomic_write(&status_path, bytes),
            None => std::fs::remove_file(&status_path).map_err(Into::into),
        };
        error
    };
    let config = config::load_named(paths, &selected.name).map_err(restore)?;
    let mut tree = ProcessTree::spawn(&config, true).map_err(restore)?;
    status.pid = tree.child.id();
    atomic_write(&paths.status(&config.name), &serde_json::to_vec(&status)?)?;
    let mut interval = tokio::time::interval(Duration::from_millis(150));
    let (exit, mut interrupted) = loop {
        tokio::select! {
            _ = &mut signal => break (None, true),
            exit = tree.child.wait() => break (Some(exit?), false),
            _ = interval.tick() => tree.refresh()?,
        }
    };
    #[cfg(unix)]
    {
        interrupted |= exit.is_some_and(|exit| {
            let (code, signal) = process::exit_details(exit);
            signal == Some(libc::SIGINT) || code == Some(130)
        });
    }
    let _ = &mut interrupted;
    let stopped = tree
        .stop(Duration::from_millis(config.stop_timeout_ms), interrupted)
        .await?;
    let exit = exit.unwrap_or(stopped);
    let (code, signal) = process::exit_details(exit);
    let final_status = RuntimeStatus {
        state: if exit.success() || interrupted {
            ServiceState::Stopped
        } else {
            ServiceState::Failed
        },
        last_exit_code: code,
        last_exit_signal: signal,
        ..Default::default()
    };
    atomic_write(
        &paths.status(&config.name),
        &serde_json::to_vec(&final_status)?,
    )?;
    Ok(if interrupted {
        130
    } else {
        code.unwrap_or_else(|| 128 + signal.unwrap_or(1))
    })
}

async fn daemon_command(paths: Paths, command: DaemonCommand) -> Result<i32> {
    match command {
        DaemonCommand::Serve {
            source_executable,
            prepared_runtime,
        } => {
            #[cfg(any(unix, windows))]
            {
                if let Some(source) = source_executable {
                    return crate::runtime::serve_registered(&paths, &source).await;
                }
                // ensure / OS 起動役から渡されたコピーは、起動前に prepare 済み。
                if !prepared_runtime && !crate::runtime::is_current_executable(&paths)? {
                    // 直接 serve を指定してもインストール先の実行ファイルを常駐させない。
                    // 別の保存先の daemon が稼働中なら、従来どおり何もせず終了する。
                    let Some(lock) = Lock::try_acquire(&paths.daemon_lock())? else {
                        return Ok(0);
                    };
                    drop(lock);
                    daemon::ensure(&paths).await?;
                    return Ok(0);
                }
            }
            #[cfg(not(any(unix, windows)))]
            if source_executable.is_some() || prepared_runtime {
                return fail(
                    "UNSUPPORTED_OPTION",
                    "--source-executable is only used on supported platforms",
                );
            }
            daemon::serve(paths).await?;
        }
        DaemonCommand::Install { dry_run } => {
            if dry_run {
                print!("{}", platform::render(&paths, &std::env::current_exe()?)?);
            } else {
                platform::install(&paths).await?;
                daemon::ensure(&paths).await?;
                println!("Daemon registered with {}", platform::kind());
            }
        }
        DaemonCommand::Uninstall => {
            if ipc::ping(&paths).await {
                ipc::request(&paths, Command::Shutdown).await?;
            }
            if platform::registered(&paths)? {
                platform::uninstall(&paths).await?;
            }
            println!("Daemon registration removed");
        }
        DaemonCommand::Start => {
            daemon::ensure(&paths).await?;
            println!("Daemon is running");
        }
        DaemonCommand::Stop => {
            if ipc::ping(&paths).await {
                ipc::request(&paths, Command::Shutdown).await?;
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            let mut next_ping = tokio::time::Instant::now() + Duration::from_secs(1);
            while tokio::time::Instant::now() < deadline {
                if Lock::try_acquire(&paths.daemon_lock())?.is_some() {
                    println!("Daemon stopped");
                    return Ok(0);
                }
                // 登録直後に OS の起動役が遅れて起動した daemon は、停止後にロックを取ることがある。
                // 停止要求に応答した daemon は接続を受け付けないため、応答するのは後から起動した daemon。
                if tokio::time::Instant::now() >= next_ping {
                    if ipc::ping(&paths).await {
                        let _ = ipc::request(&paths, Command::Shutdown).await;
                    }
                    next_ping = tokio::time::Instant::now() + Duration::from_secs(1);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            return fail(
                "DAEMON_UNAVAILABLE",
                "The user daemon is still active; it may be using another storage directory or have unavailable IPC",
            );
        }
        DaemonCommand::Status { json } => {
            let running = ipc::ping(&paths).await;
            let registered = platform::registered(&paths)?;
            if json {
                println!(
                    "{}",
                    serde_json::json!({"schema_version":1,"running":running,"registered":registered,"integration":platform::kind(),"home":paths.home})
                );
            } else {
                println!(
                    "Daemon:        {}\nRegistered:    {}\nIntegration:   {}\nStorage:       {}",
                    if running { "running" } else { "stopped" },
                    if registered { "yes" } else { "no" },
                    platform::kind(),
                    readable_path(&paths.home).display()
                );
            }
        }
    }
    Ok(0)
}

#[derive(Serialize)]
struct DoctorCheck {
    check: String,
    level: &'static str,
    detail: String,
    service: Option<String>,
}

async fn doctor(paths: &Paths, json: bool) -> Result<i32> {
    let mut checks = Vec::new();
    let mut push = |check: &str, level, detail: String, service: Option<String>| {
        checks.push(DoctorCheck {
            check: check.into(),
            level,
            detail,
            service,
        })
    };
    let registered = platform::registered(paths)?;
    push(
        "daemon-registration",
        if registered { "ok" } else { "warning" },
        readable_path(&platform::registration_path(paths)?)
            .display()
            .to_string(),
        None,
    );
    let running = ipc::ping(paths).await;
    let locked = Lock::try_acquire(&paths.daemon_lock())?.is_none();
    push(
        "daemon-state",
        if running {
            "ok"
        } else if locked {
            "error"
        } else {
            "warning"
        },
        if running {
            "running"
        } else if locked {
            "Daemon lock is held but IPC is unavailable"
        } else {
            "stopped"
        }
        .into(),
        None,
    );
    push(
        "ipc",
        if running { "ok" } else { "warning" },
        format!("{:?}", paths.endpoint()),
        None,
    );
    for (name, dir) in [
        ("config-directory", &paths.configs),
        ("log-directory", &paths.logs),
    ] {
        push(
            name,
            if fs::read_dir(dir).is_ok() {
                "ok"
            } else {
                "error"
            },
            readable_path(dir).display().to_string(),
            None,
        );
    }
    for path in config::config_files(paths)? {
        match config::load(&path) {
            Ok(config) => {
                push(
                    "config",
                    "ok",
                    readable_path(&path).display().to_string(),
                    Some(config.name.clone()),
                );
                push(
                    "working-directory",
                    if config.cwd.is_dir() { "ok" } else { "error" },
                    readable_path(&config.cwd).display().to_string(),
                    Some(config.name.clone()),
                );
                push(
                    "executable",
                    if resolve::executable::is_executable(&config.resolved_executable) {
                        "ok"
                    } else {
                        "error"
                    },
                    readable_path(&config.resolved_executable)
                        .display()
                        .to_string(),
                    Some(config.name.clone()),
                );
                if let Some(script) = &config.resolved_script {
                    push(
                        "entrypoint",
                        if script.is_file() { "ok" } else { "error" },
                        readable_path(script).display().to_string(),
                        Some(config.name.clone()),
                    );
                }
                if let Some(path) = &config.env_file {
                    push(
                        "env-file",
                        if config.effective_environment().is_ok() {
                            "ok"
                        } else {
                            "error"
                        },
                        readable_path(path).display().to_string(),
                        Some(config.name),
                    );
                }
            }
            Err(error) => push("config", "error", format!("{error:#}"), None),
        }
    }
    if registered {
        let result = platform::integration_status(paths).await;
        push(
            "os-integration",
            if result.is_ok() { "ok" } else { "error" },
            result
                .map(|_| format!("{} registration is accessible", platform::kind()))
                .unwrap_or_else(|e| format!("{e:#}")),
            None,
        );
    } else {
        push(
            "os-integration",
            "warning",
            format!(
                "Not installed; enable a service or run svcnest daemon install ({})",
                platform::kind()
            ),
            None,
        );
    }
    let failed = checks.iter().any(|check| check.level == "error");
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"checks":checks}))?
        );
    } else {
        for check in checks {
            println!(
                "{:<7} {:<22} {}{}",
                check.level.to_uppercase(),
                check.check,
                check
                    .service
                    .map(|name| format!("[{name}] "))
                    .unwrap_or_default(),
                check.detail
            );
        }
    }
    Ok(i32::from(failed))
}
