#[cfg(unix)]
use std::process::Child;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use svcnest::{
    config,
    ipc::{self, Action, ServiceState, Snapshot, Target},
    paths::Paths,
};

const CLI: &str = env!("CARGO_BIN_EXE_svcnest");
static E2E_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Sandbox {
    _serial: std::sync::MutexGuard<'static, ()>,
    temp: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
    paths: Paths,
}

impl Sandbox {
    fn new() -> Self {
        // daemon はユーザー単位で一つ。個別の保存先を使うケースも直列で実行する。
        let serial = E2E_SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("state");
        let project = temp.path().join("project with spaces");
        fs::create_dir(&project).unwrap();
        let paths = Paths::discover(Some(home.clone())).unwrap();
        Self {
            _serial: serial,
            temp,
            home,
            project,
            paths,
        }
    }
    fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(CLI);
        command.arg("--home").arg(&self.home).current_dir(cwd);
        command
    }
    fn output_at(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut command = self.command(cwd);
        command.args(args);
        output_with_timeout(&mut command, Duration::from_secs(30))
    }
    fn output(&self, args: &[&str]) -> Output {
        self.output_at(&self.project, args)
    }
    fn ok_at(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.output_at(cwd, args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        self.ok_at(&self.project, args)
    }
    fn error(&self, args: &[&str], code: &str) {
        let output = self.output(args);
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(code),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn add(&self, name: &str, mode: &str, restart: &str, cwd: &Path) {
        let probe = std::env::current_exe().unwrap();
        let pids = self.temp.path().join(format!("{name}-pids"));
        let lock = self.temp.path().join(format!("{name}-instance.lock"));
        self.ok_at(
            cwd,
            &[
                "add",
                name,
                "--restart",
                restart,
                "--stop-timeout",
                "300ms",
                "--env",
                &format!("SVCNEST_E2E_MODE={mode}"),
                "--env",
                &format!("SVCNEST_E2E_PIDS={}", pids.display()),
                "--env",
                &format!("SVCNEST_E2E_LOCK={}", lock.display()),
                "--",
                probe.to_str().unwrap(),
                "--exact",
                "fixture",
                "--nocapture",
                "--skip",
                "literal spaces $HOME; $(echo injection) | &",
            ],
        );
    }
    fn snapshot(&self, args: &[&str]) -> Snapshot {
        serde_json::from_str(&self.ok(args)).unwrap()
    }
    fn pid(&self, name: &str) -> u32 {
        self.snapshot(&["status", name, "--json"]).services[0]
            .runtime
            .pid
            .unwrap()
    }
    fn pids(&self, name: &str) -> Vec<u32> {
        fs::read_to_string(self.temp.path().join(format!("{name}-pids")))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_whitespace().next()?.parse().ok())
            .collect()
    }
    fn wait_pids(&self, name: &str, count: usize) -> Vec<u32> {
        wait_for(|| {
            let pids = self.pids(name);
            (pids.len() >= count).then_some(pids)
        })
    }
    fn state(&self, name: &str) -> ServiceState {
        self.snapshot(&["status", name, "--json"]).services[0]
            .runtime
            .state
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let mut command = self.command(&self.project);
        command
            .args(["daemon", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = status_with_timeout(&mut command, Duration::from_secs(30));
        #[cfg(unix)]
        let _ = fs::remove_dir_all(&self.paths.runtime);
    }
}

fn status_with_timeout(command: &mut Command, timeout: Duration) -> Option<ExitStatus> {
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().ok()? {
            return Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn output_with_timeout(command: &mut Command, timeout: Duration) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let description = format!("{command:?}");
    let mut child = command.spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + timeout;
    loop {
        let status = child.try_wait().unwrap();
        if let Some(status) = status
            && stdout_reader.is_finished()
            && stderr_reader.is_finished()
        {
            return Output {
                status,
                stdout: stdout_reader.join().unwrap(),
                stderr: stderr_reader.join().unwrap(),
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            // 子孫がパイプを保持していても、読み取りスレッドの join で待ち続けない。
            panic!(
                "Timed out waiting for e2e command or output: {description}; status: {status:?}; stdout finished: {}; stderr finished: {}",
                stdout_reader.is_finished(),
                stderr_reader.is_finished(),
            );
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for<T>(mut condition: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(value) = condition() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "Timed out waiting for subprocess state"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        svcnest::process::unix::is_pid_alive(pid)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0;
            let alive = GetExitCodeProcess(handle, &mut code) != 0 && code == 259;
            CloseHandle(handle);
            alive
        }
    }
}

fn force_kill(pid: u32) {
    #[cfg(unix)]
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGKILL) }, 0);
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
        };
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            assert!(!handle.is_null());
            assert_ne!(TerminateProcess(handle, 1), 0);
            CloseHandle(handle);
        }
    }
}

#[test]
fn fixture() {
    let Ok(mode) = std::env::var("SVCNEST_E2E_MODE") else {
        return;
    };
    let mut pid_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var_os("SVCNEST_E2E_PIDS").unwrap())
        .unwrap();
    writeln!(pid_file, "{} {mode}", std::process::id()).unwrap();
    let _instance_lock = if !mode.contains("child") {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(std::env::var_os("SVCNEST_E2E_LOCK").unwrap())
            .unwrap();
        if file.try_lock().is_err() {
            eprintln!("OVERLAPPING_INSTANCE");
            std::process::exit(42);
        }
        Some(file)
    } else {
        None
    };
    println!(
        "probe stdout: mode={mode} cwd={} argv={:?} VALUE={}",
        std::env::current_dir().unwrap().display(),
        std::env::args().collect::<Vec<_>>(),
        std::env::var("SVCNEST_TEST_VALUE").unwrap_or_default()
    );
    eprintln!("probe stderr");
    if mode == "fail" {
        std::process::exit(7);
    }
    #[cfg(windows)]
    if matches!(mode.as_str(), "console" | "console-child") {
        windowless_console_fixture(&mode);
        return;
    }
    if mode == "path-parent" {
        let child = if cfg!(windows) {
            "path-probe.exe"
        } else {
            "path-probe"
        };
        let status = Command::new(child)
            .args(["--exact", "fixture", "--nocapture"])
            .env("SVCNEST_E2E_MODE", "once-child")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    if mode == "once" || mode == "once-child" {
        return;
    }
    if mode == "interactive" {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).unwrap();
        println!("stdin received: {}", line.trim());
        std::io::stdout().flush().unwrap();
    }
    #[cfg(unix)]
    if mode.starts_with("tree") {
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_IGN);
        }
    }
    if matches!(mode.as_str(), "tree" | "tree-child") {
        let child_mode = if mode == "tree" {
            "tree-child"
        } else {
            "tree-grandchild"
        };
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "fixture", "--nocapture"])
            .env("SVCNEST_E2E_MODE", child_mode)
            .spawn()
            .unwrap();
        let _ = thread::spawn(move || {
            let _ = child.wait();
        });
    }
    if mode == "brief" {
        thread::sleep(Duration::from_millis(500));
        return;
    }
    if mode == "flood" {
        let gate = PathBuf::from(std::env::var_os("SVCNEST_TEST_GATE").unwrap());
        while !gate.exists() {
            println!("flood waiting");
            std::io::stdout().flush().unwrap();
            thread::sleep(Duration::from_millis(100));
        }
        let payload = "x".repeat(32 * 1024);
        for index in 0..800 {
            println!("flood-record-{index:04} {payload}");
        }
        println!("FLOOD-END");
        std::io::stdout().flush().unwrap();
    }
    loop {
        println!("probe heartbeat");
        std::io::stdout().flush().unwrap();
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(windows)]
fn windowless_console_fixture(mode: &str) {
    use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut signal = tokio::signal::windows::ctrl_break().unwrap();
        let mut child = (mode == "console").then(|| {
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "fixture", "--nocapture"])
                .env("SVCNEST_E2E_MODE", "console-child")
                .spawn()
                .unwrap()
        });
        let path = PathBuf::from(std::env::var_os("SVCNEST_E2E_PIDS").unwrap());
        if child.is_some() {
            // CreateProcess の復帰時点では子の console 接続がまだ完了していない場合がある。
            let ready = path.with_extension("console-child.json");
            wait_for(|| ready.is_file().then_some(()));
        }
        let mut pids = [0u32; 16];
        let count = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
        assert!(count > 0 && count as usize <= pids.len());
        let report = serde_json::json!({
            "has_console_window": !unsafe { GetConsoleWindow() }.is_null(),
            "console_processes": &pids[..count as usize],
        });
        fs::write(
            path.with_extension(format!("{mode}.json")),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        signal.recv().await.unwrap();
        if let Some(child) = child.as_mut() {
            assert!(child.wait().unwrap().success());
        }
        println!("console-break-received: mode={mode}");
    });
}

#[cfg(windows)]
#[test]
fn windows_background_tree_has_no_console_window_and_stops_gracefully() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "console", "never", &sandbox.project);
    let mut config = config::load_named(&sandbox.paths, "api").unwrap();
    config.stop_timeout_ms = 2000;
    config::save(&sandbox.paths, &config).unwrap();
    sandbox.ok(&["start"]);
    let pids = sandbox.wait_pids("api", 2);
    for mode in ["console", "console-child"] {
        let path = sandbox
            .temp
            .path()
            .join("api-pids")
            .with_extension(format!("{mode}.json"));
        let report: serde_json::Value = wait_for(|| {
            fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        });
        assert_eq!(report["has_console_window"], false, "{mode}: {report}");
        let console_processes = report["console_processes"].as_array().unwrap();
        // runner、対象、子が同じ画面なしの console を共有している。
        assert!(console_processes.len() >= 3, "{mode}: {report}");
        assert!(pids.iter().all(|pid| {
            console_processes
                .iter()
                .any(|value| value.as_u64() == Some(u64::from(*pid)))
        }));
    }
    sandbox.ok(&["stop"]);
    let status = sandbox.snapshot(&["status", "api", "--json"]);
    assert_eq!(status.services[0].runtime.state, ServiceState::Stopped);
    assert_eq!(status.services[0].runtime.last_exit_code, Some(0));
    assert!(pids.iter().all(|pid| !is_alive(*pid)));
    let logs = sandbox.ok(&["logs", "api", "-n", "100"]);
    for mode in ["console", "console-child"] {
        assert!(logs.contains(&format!("console-break-received: mode={mode}")));
    }
}

#[test]
fn project_workflow_resolves_children_and_preserves_registration_environment() {
    let sandbox = Sandbox::new();
    let child = sandbox.project.join("src/routes");
    fs::create_dir_all(&child).unwrap();
    let env_file = sandbox.project.join(".env");
    fs::write(
        &env_file,
        "SVCNEST_TEST_VALUE=from-file\nONLY_FILE=not-persisted\n",
    )
    .unwrap();
    let probe = std::env::current_exe().unwrap();
    let mut command = sandbox.command(&sandbox.project);
    command
        .env("API_KEY", "never-persist-this")
        .args([
            "add",
            "api",
            "--env-file",
            ".env",
            "--env",
            "SVCNEST_TEST_VALUE=explicit",
            "--env",
            "SVCNEST_E2E_MODE=once",
            "--env",
            &format!(
                "SVCNEST_E2E_PIDS={}",
                sandbox.temp.path().join("api-pids").display()
            ),
            "--env",
            &format!(
                "SVCNEST_E2E_LOCK={}",
                sandbox.temp.path().join("api-instance.lock").display()
            ),
            "--restart",
            "never",
            "--",
            probe.to_str().unwrap(),
            "--exact",
            "fixture",
            "--nocapture",
        ])
        .env("SVCNEST_E2E_MODE", "not-captured");
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = config::load_named(&sandbox.paths, "api").unwrap();
    assert_eq!(config.cwd, fs::canonicalize(&sandbox.project).unwrap());
    assert_eq!(config.resolved_executable, fs::canonicalize(probe).unwrap());
    assert!(config.environment.contains_key("PATH"));
    let saved = fs::read_to_string(sandbox.paths.config("api")).unwrap();
    assert!(!saved.contains("never-persist-this"));
    assert!(!saved.contains("not-persisted"));
    assert!(!saved.contains("not-captured"));
    let text = sandbox.ok(&["config", "show"]);
    assert!(!text.contains("explicit"));
    assert!(text.contains("********"));
    assert!(
        sandbox
            .ok(&["config", "show", "--show-secrets"])
            .contains("explicit")
    );
    assert_eq!(
        sandbox.ok_at(&child, &["config", "path"]).trim(),
        sandbox.paths.config("api").to_str().unwrap()
    );
    assert_eq!(
        serde_json::from_str::<Snapshot>(&sandbox.ok_at(&child, &["status", "--json"]))
            .unwrap()
            .services[0]
            .name,
        "api"
    );
    sandbox.ok_at(&child, &["start"]);
    wait_for(|| {
        fs::read_to_string(sandbox.paths.log("api"))
            .ok()
            .filter(|text| text.contains("probe stderr"))
    });
    let logs = sandbox.ok_at(&child, &["logs", "-n", "100"]);
    assert!(logs.contains("stdout |"));
    assert!(logs.contains("stderr |"));
    sandbox.ok_at(&child, &["stop"]);
    assert_eq!(sandbox.state("api"), ServiceState::Stopped);
    assert_eq!(sandbox.snapshot(&["list", "--json"]).schema_version, 1);
    sandbox.error(
        &["add", "api", "--", "missing-program"],
        "EXECUTABLE_NOT_FOUND",
    );
}

#[test]
fn relative_executable_and_explicit_name_work_from_another_directory() {
    let sandbox = Sandbox::new();
    let probe_name = if cfg!(windows) { "probe.exe" } else { "probe" };
    fs::copy(
        std::env::current_exe().unwrap(),
        sandbox.project.join(probe_name),
    )
    .unwrap();
    sandbox.ok(&[
        "add",
        "backend",
        "--restart",
        "never",
        "--",
        &format!("./{probe_name}"),
        "--exact",
        "fixture",
    ]);
    let config = config::load_named(&sandbox.paths, "backend").unwrap();
    assert_eq!(config.command[0], format!("./{probe_name}"));
    assert_eq!(
        config.resolved_executable,
        fs::canonicalize(sandbox.project.join(probe_name)).unwrap()
    );
    sandbox.ok_at(sandbox.temp.path(), &["start", "backend"]);
    sandbox.ok_at(sandbox.temp.path(), &["status", "backend"]);
    sandbox.error(&["start", "missing-service"], "SERVICE_NOT_FOUND");
}

#[test]
fn registered_path_drives_the_executable_and_its_children_independently_of_daemon_path() {
    let sandbox = Sandbox::new();
    sandbox.ok(&["daemon", "start"]);
    let bin = sandbox.temp.path().join("registration-only-bin");
    fs::create_dir(&bin).unwrap();
    let parent = if cfg!(windows) {
        "parent-probe.exe"
    } else {
        "parent-probe"
    };
    let child = if cfg!(windows) {
        "path-probe.exe"
    } else {
        "path-probe"
    };
    for name in [parent, child] {
        fs::copy(std::env::current_exe().unwrap(), bin.join(name)).unwrap();
    }
    let output = sandbox
        .command(&sandbox.project)
        .env("PATH", &bin)
        .args([
            "add",
            "api",
            "--restart",
            "never",
            "--env",
            "SVCNEST_E2E_MODE=path-parent",
            "--env",
            &format!(
                "SVCNEST_E2E_PIDS={}",
                sandbox.temp.path().join("api-pids").display()
            ),
            "--env",
            &format!(
                "SVCNEST_E2E_LOCK={}",
                sandbox.temp.path().join("api-instance.lock").display()
            ),
            "--",
            parent,
            "--exact",
            "fixture",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = config::load_named(&sandbox.paths, "api").unwrap();
    assert_eq!(config.command[0], parent);
    assert_eq!(
        config.resolved_executable,
        fs::canonicalize(bin.join(parent)).unwrap()
    );
    sandbox.ok(&["start"]);
    sandbox.wait_pids("api", 2);
    wait_for(|| (sandbox.state("api") == ServiceState::Stopped).then_some(()));
    let logs = sandbox.ok(&["logs"]);
    assert!(logs.contains("once-child"));
}

#[cfg(windows)]
#[test]
fn windows_batches_register_directly_and_keep_literal_arguments_in_both_run_modes() {
    let sandbox = Sandbox::new();
    let node = svcnest::resolve::executable::resolve(
        "node",
        &sandbox.project,
        std::env::var_os("PATH").as_deref(),
        None,
    )
    .unwrap();
    fs::copy(node, sandbox.project.join("node.exe")).unwrap();
    fs::write(
        sandbox.project.join("argv.js"),
        "console.log(JSON.stringify({argv: process.argv.slice(2), cwd: process.cwd()}));",
    )
    .unwrap();
    let arguments = [
        "with spaces",
        "",
        "& | < > ^ ! %TOKEN%",
        "\"quoted\"",
        "日本語",
        "trailing\\",
        "\" & echo injected > injected.txt & rem \"",
    ];
    let other = sandbox.temp.path().join("another directory");
    fs::create_dir(&other).unwrap();
    for extension in ["bat", "CMD"] {
        let input = format!(".\\start script.{extension}");
        let name = extension.to_ascii_lowercase();
        fs::write(
            sandbox.project.join(format!("start script.{extension}")),
            "@echo off\r\n\"%~dp0node.exe\" \"%~dp0argv.js\" %*\r\n",
        )
        .unwrap();
        let mut registration = vec!["add", &name, "--restart", "never", "--", &input];
        registration.extend(arguments);
        sandbox.ok(&registration);
        let config = config::load_named(&sandbox.paths, &name).unwrap();
        assert_eq!(config.command[0], input);
        assert_eq!(
            config.resolved_executable,
            fs::canonicalize(sandbox.project.join(format!("start script.{extension}"))).unwrap()
        );
        assert!(config.resolved_script.is_none());
        let output = sandbox.ok_at(&other, &["run", &name]);
        let result: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(result["argv"], serde_json::json!(arguments));
        assert_eq!(
            fs::canonicalize(result["cwd"].as_str().unwrap()).unwrap(),
            fs::canonicalize(&sandbox.project).unwrap()
        );
        sandbox.ok_at(&other, &["start", &name]);
        wait_for(|| (sandbox.state(&name) == ServiceState::Stopped).then_some(()));
        let logs = sandbox.ok(&["logs", &name]);
        assert!(logs.contains(&serde_json::to_string(&arguments).unwrap()));
        assert!(!sandbox.project.join("injected.txt").exists());
    }
    sandbox.error(
        &[
            "add",
            "invalid",
            "--shell",
            "pwsh",
            "--",
            ".\\start script.bat",
        ],
        "INVALID_SHELL",
    );
}

#[cfg(windows)]
#[test]
fn windows_ps1_uses_the_registering_powershell_and_pins_its_environment() {
    let sandbox = Sandbox::new();
    // 登録元とは異なる環境の daemon が既に動いていても、登録時のシェルを選ぶ。
    sandbox.ok(&["daemon", "start"]);
    let input = ".\\start script.PS1";
    fs::write(sandbox.project.join("start script.PS1"), "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)\n@{ argv = @($args); cwd = (Get-Location).Path; shell = (Get-Process -Id $PID).Path; policy = (Get-ExecutionPolicy -Scope Process).ToString() } | ConvertTo-Json -Compress\n").unwrap();
    let arguments = [
        "with spaces",
        "$HOME; $(Set-Content injected.txt bad) | &",
        "%TOKEN%",
        "日本語",
    ];
    let other = sandbox.temp.path().join("another directory");
    fs::create_dir(&other).unwrap();
    let mut tested = 0;
    for command in ["powershell.exe", "pwsh.exe"] {
        let Ok(shell) = svcnest::resolve::executable::resolve(
            command,
            &sandbox.project,
            std::env::var_os("PATH").as_deref(),
            None,
        ) else {
            continue;
        };
        tested += 1;
        let name = if command == "pwsh.exe" {
            "pwsh"
        } else {
            "powershell"
        };
        // CLI を実際の PowerShell から呼び、PATH にシェルがなくても同じ実体を保存する。
        let mut argv = vec![
            CLI,
            "--home",
            sandbox.home.to_str().unwrap(),
            "add",
            name,
            "--restart",
            "never",
            "--",
            input,
        ];
        argv.extend(arguments);
        let invocation = format!(
            "& {}; exit $LASTEXITCODE",
            argv.iter()
                .map(|arg| format!("'{}'", arg.replace('\'', "''")))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let mut parent = Command::new(&shell);
        parent
            .current_dir(&sandbox.project)
            .env_remove("PSModulePath")
            .env("PATH", &sandbox.project)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-ExecutionPolicy",
                "RemoteSigned",
                "-Command",
                &invocation,
            ]);
        let output = output_with_timeout(&mut parent, Duration::from_secs(30));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let config = config::load_named(&sandbox.paths, name).unwrap();
        assert_eq!(config.resolved_executable, shell);
        assert_eq!(config.command[0], input);
        assert_eq!(
            config.resolved_script.as_ref().unwrap(),
            &fs::canonicalize(sandbox.project.join("start script.PS1")).unwrap()
        );
        assert_eq!(
            config.interpreter_environment["PSExecutionPolicyPreference"],
            "RemoteSigned"
        );
        assert!(config.interpreter_environment.contains_key("PSModulePath"));
        let execution = sandbox.output_at(&other, &["run", name]);
        assert!(
            execution.status.success() && !execution.stdout.is_empty(),
            "{name}: stdout={} stderr={}",
            String::from_utf8_lossy(&execution.stdout),
            String::from_utf8_lossy(&execution.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&execution.stdout).unwrap();
        assert_eq!(result["argv"], serde_json::json!(arguments));
        assert_eq!(result["policy"], "RemoteSigned");
        assert_eq!(
            fs::canonicalize(result["shell"].as_str().unwrap()).unwrap(),
            shell
        );
        assert_eq!(
            fs::canonicalize(result["cwd"].as_str().unwrap()).unwrap(),
            fs::canonicalize(&sandbox.project).unwrap()
        );
        sandbox.ok(&["start", name]);
        wait_for(|| (sandbox.state(name) == ServiceState::Stopped).then_some(()));
        let logs = sandbox.ok(&["logs", name]);
        let result: serde_json::Value = logs
            .lines()
            .find_map(|line| {
                let start = line.find('{')?;
                serde_json::from_str(&line[start..]).ok()
            })
            .unwrap_or_else(|| panic!("Missing script output: {logs}"));
        assert_eq!(result["argv"], serde_json::json!(arguments));
        assert_eq!(result["policy"], "RemoteSigned");
        assert!(!sandbox.project.join("injected.txt").exists());
    }
    assert!(tested > 0, "Windows PowerShell is required for this test");
    let shell = svcnest::resolve::executable::resolve(
        "powershell.exe",
        &sandbox.project,
        std::env::var_os("PATH").as_deref(),
        None,
    )
    .unwrap();
    let literal_arguments = ["", "\"quoted\"", "trailing\\", "-flag", "first\r\nsecond"];
    let mut registration = vec![
        "add",
        "explicit",
        "--shell",
        shell.to_str().unwrap(),
        "--env",
        "PSExecutionPolicyPreference=RemoteSigned",
        "--",
        input,
    ];
    registration.extend(literal_arguments);
    sandbox.ok(&registration);
    assert_eq!(
        config::load_named(&sandbox.paths, "explicit")
            .unwrap()
            .resolved_executable,
        shell
    );
    let result: serde_json::Value =
        serde_json::from_str(sandbox.ok(&["run", "explicit"]).trim()).unwrap();
    assert_eq!(result["policy"], "RemoteSigned");
    assert_eq!(result["argv"], serde_json::json!(literal_arguments));
    sandbox.error(
        &["add", "invalid", "--shell", "cmd.exe", "--", input],
        "INVALID_SHELL",
    );
}

#[cfg(windows)]
#[test]
fn windows_npx_uses_node_directly_and_preserves_literal_arguments() {
    let sandbox = Sandbox::new();
    let node = svcnest::resolve::executable::resolve(
        "node",
        &sandbox.project,
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("PATHEXT").as_deref(),
    )
    .unwrap();
    let bin = sandbox.temp.path().join("npm launchers with spaces");
    fs::create_dir(&bin).unwrap();
    fs::copy(&node, bin.join("node.exe")).unwrap();
    let bundled = bin.join("node_modules/npm/bin");
    fs::create_dir_all(&bundled).unwrap();
    let global = sandbox.temp.path().join("global prefix");
    let global_bin = global.join("node_modules/npm/bin");
    fs::create_dir_all(&global_bin).unwrap();
    fs::write(
        bundled.join("npm-prefix.js"),
        format!(
            "console.log({});",
            serde_json::to_string(global.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    fs::write(
        bundled.join("npx-cli.js"),
        "throw new Error('incorrect bundled entrypoint');",
    )
    .unwrap();
    fs::write(global_bin.join("npx-cli.js"), "console.log('argv=' + JSON.stringify(process.argv.slice(2))); setInterval(() => {}, 1000);").unwrap();
    fs::write(bin.join("npx.cmd"), "SET \"NODE_EXE=%~dp0\\node.exe\"\nSET \"NPM_PREFIX_JS=%~dp0\\node_modules\\npm\\bin\\npm-prefix.js\"\nSET \"NPX_CLI_JS=%~dp0\\node_modules\\npm\\bin\\npx-cli.js\"\n\"%NODE_EXE%\" \"%NPX_CLI_JS%\" %*\n").unwrap();
    let literal = "space & | %TOKEN% \"quoted\" 日本語";
    let output = sandbox
        .command(&sandbox.project)
        .env("PATH", &bin)
        .args([
            "add",
            "api",
            "--restart",
            "never",
            "--",
            "npx",
            "some-mcp-server",
            literal,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let config = config::load_named(&sandbox.paths, "api").unwrap();
    assert_eq!(config.command, ["npx", "some-mcp-server", literal]);
    assert_eq!(
        config.resolved_executable,
        fs::canonicalize(bin.join("node.exe")).unwrap()
    );
    assert_eq!(
        config.resolved_script.as_ref().unwrap(),
        &fs::canonicalize(global_bin.join("npx-cli.js")).unwrap()
    );
    sandbox.ok(&["start"]);
    let logs = wait_for(|| {
        let text = sandbox.ok(&["logs"]);
        text.contains("argv=").then_some(text)
    });
    let line = logs
        .lines()
        .find(|line| line.contains("argv="))
        .unwrap()
        .split_once("argv=")
        .unwrap()
        .1;
    let args: Vec<String> = serde_json::from_str(line).unwrap();
    assert_eq!(args, ["some-mcp-server", literal]);
    sandbox.ok(&["stop"]);
    // 実際にインストールされた npm の標準 npx ランチャーも確認する。
    sandbox.ok(&[
        "add",
        "api",
        "--replace",
        "--restart",
        "never",
        "--",
        "npx",
        "--version",
    ]);
    sandbox.ok(&["start"]);
    wait_for(|| (sandbox.state("api") == ServiceState::Stopped).then_some(()));
    assert!(
        config::load_named(&sandbox.paths, "api")
            .unwrap()
            .resolved_script
            .is_some()
    );
}

#[test]
fn multiple_services_are_ambiguous_and_all_never_selects_global_services() {
    let sandbox = Sandbox::new();
    let other = sandbox.temp.path().join("other");
    fs::create_dir(&other).unwrap();
    sandbox.add("api", "long", "never", &sandbox.project);
    sandbox.add("worker", "long", "never", &sandbox.project);
    sandbox.add("elsewhere", "long", "never", &other);
    sandbox.error(&["start"], "AMBIGUOUS_SERVICE");
    sandbox.ok(&["start", "--all"]);
    assert_eq!(sandbox.state("api"), ServiceState::Running);
    assert_eq!(sandbox.state("worker"), ServiceState::Running);
    assert_eq!(sandbox.state("elsewhere"), ServiceState::Stopped);
    sandbox.error(&["run", "api"], "SERVICE_RUNNING");
    sandbox.error(&["remove", "api"], "SERVICE_RUNNING");
    let probe = std::env::current_exe().unwrap();
    sandbox.error(
        &[
            "add",
            "api",
            "--replace",
            "--",
            probe.to_str().unwrap(),
            "--exact",
            "fixture",
        ],
        "SERVICE_RUNNING",
    );
    let services = sandbox.snapshot(&["status", "--all", "--json"]).services;
    assert_eq!(services.len(), 2);
    sandbox.ok(&["remove", "--all", "--stop", "--purge"]);
    assert!(!sandbox.paths.log("api").exists());
    assert!(!sandbox.paths.config("worker").exists());
    assert_eq!(sandbox.snapshot(&["list", "--json"]).services.len(), 1);
}

#[test]
fn concurrent_starts_and_daemons_cannot_duplicate_a_service() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "long", "always", &sandbox.project);
    let mut children = (0..8)
        .map(|_| {
            sandbox
                .command(&sandbox.project)
                .args(["start"])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for child in children.drain(..) {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let first = sandbox.pid("api");
    sandbox.ok(&["start"]);
    assert_eq!(sandbox.pid("api"), first);
    assert_eq!(sandbox.wait_pids("api", 1).len(), 1);
    let daemon_pid = fs::read_to_string(sandbox.paths.runtime.join("daemon.pid")).unwrap();
    sandbox.ok(&["daemon", "serve"]);
    assert_eq!(
        fs::read_to_string(sandbox.paths.runtime.join("daemon.pid")).unwrap(),
        daemon_pid
    );
    sandbox.ok(&["stop"]);
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(sandbox.state("api"), ServiceState::Stopped);
    assert_eq!(sandbox.pids("api").len(), 1);
}

#[test]
fn daemon_start_closes_cli_output_while_daemon_is_running() {
    let sandbox = Sandbox::new();
    let mut command = sandbox.command(&sandbox.project);
    command.args(["daemon", "start"]);
    let output = output_with_timeout(&mut command, Duration::from_secs(5));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Daemon is running"));
    let pid = fs::read_to_string(sandbox.paths.runtime.join("daemon.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(is_alive(pid));
}

#[test]
fn a_user_cannot_start_another_daemon_by_changing_storage_directory() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "long", "never", &sandbox.project);
    sandbox.ok(&["start"]);
    let daemon_pid = fs::read_to_string(sandbox.paths.runtime.join("daemon.pid")).unwrap();
    let other_home = sandbox.temp.path().join("other-state");
    let other_paths = Paths::discover(Some(other_home.clone())).unwrap();
    assert_ne!(sandbox.paths.runtime, other_paths.runtime);
    assert_eq!(sandbox.paths.daemon_lock(), other_paths.daemon_lock());
    let mut command = Command::new(CLI);
    command
        .arg("--home")
        .arg(&other_home)
        .args(["daemon", "serve"]);
    let output = output_with_timeout(&mut command, Duration::from_secs(5));
    assert!(output.status.success());
    assert!(!other_paths.runtime.join("daemon.pid").exists());
    assert_eq!(
        fs::read_to_string(sandbox.paths.runtime.join("daemon.pid")).unwrap(),
        daemon_pid
    );
    assert_eq!(sandbox.state("api"), ServiceState::Running);
    #[cfg(unix)]
    let _ = fs::remove_dir_all(other_paths.runtime);
}

#[test]
fn daemon_stop_reports_unavailable_ipc_instead_of_claiming_success() {
    let sandbox = Sandbox::new();
    let _lock = svcnest::paths::Lock::try_acquire(&sandbox.paths.daemon_lock())
        .unwrap()
        .unwrap();
    let output = sandbox.output(&["daemon", "stop"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("DAEMON_UNAVAILABLE"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("Daemon stopped"));
}

#[cfg(unix)]
#[test]
fn explicit_service_operations_do_not_require_a_readable_current_directory() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, process::CommandExt},
    };
    let sandbox = Sandbox::new();
    sandbox.add("api", "once", "never", &sandbox.project);
    let run = |args: &[&str]| {
        let cwd = tempfile::tempdir_in(sandbox.temp.path()).unwrap();
        let path = CString::new(cwd.path().as_os_str().as_bytes()).unwrap();
        let mut command = sandbox.command(cwd.path());
        command.args(args);
        unsafe {
            command.pre_exec(move || {
                if libc::rmdir(path.as_ptr()) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    for args in [
        &["status", "api", "--json"][..],
        &["config", "show", "api"],
        &["config", "path", "api"],
        &["start", "api"],
        &["stop", "api"],
        &["restart", "api"],
        &["stop", "api"],
        &["logs", "api"],
        &["run", "api"],
    ] {
        run(args);
    }
    let probe = std::env::current_exe().unwrap();
    run(&[
        "add",
        "backend",
        "--cwd",
        sandbox.project.to_str().unwrap(),
        "--",
        probe.to_str().unwrap(),
        "--exact",
        "fixture",
    ]);
    run(&["remove", "api", "--stop", "--purge"]);
    run(&["remove", "backend"]);
}

#[test]
fn restart_and_stop_fully_terminate_children_and_grandchildren() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "tree", "always", &sandbox.project);
    sandbox.ok(&["start"]);
    let old_pids = sandbox.wait_pids("api", 3);
    assert!(old_pids.iter().all(|pid| is_alive(*pid)));
    let first = sandbox.pid("api");
    sandbox.ok(&["restart"]);
    assert_ne!(sandbox.pid("api"), first);
    assert!(old_pids.iter().all(|pid| !is_alive(*pid)));
    let pids = sandbox.wait_pids("api", 6);
    sandbox.ok(&["stop"]);
    assert!(pids.iter().all(|pid| !is_alive(*pid)));
    assert_eq!(sandbox.state("api"), ServiceState::Stopped);
}

#[test]
fn daemon_crash_cleans_up_before_another_runner_can_start() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "tree", "always", &sandbox.project);
    sandbox.ok(&["start"]);
    let old_pids = sandbox.wait_pids("api", 3);
    let daemon_pid = fs::read_to_string(sandbox.paths.runtime.join("daemon.pid"))
        .unwrap()
        .parse()
        .unwrap();
    force_kill(daemon_pid);
    sandbox.ok(&["start"]);
    assert!(old_pids.iter().all(|pid| !is_alive(*pid)));
    let new_pids = sandbox.wait_pids("api", 6);
    assert_eq!(new_pids.len(), 6);
    assert_eq!(sandbox.state("api"), ServiceState::Running);
    assert!(
        !sandbox
            .ok(&["logs", "-n", "1000"])
            .contains("OVERLAPPING_INSTANCE")
    );
    sandbox.ok(&["stop"]);
    assert!(new_pids.iter().all(|pid| !is_alive(*pid)));
}

#[test]
fn restart_policy_applies_only_to_background_runs_and_can_be_cancelled_in_backoff() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "fail", "always", &sandbox.project);
    let foreground = sandbox.output(&["run"]);
    assert_eq!(foreground.status.code(), Some(7));
    assert!(String::from_utf8_lossy(&foreground.stdout).contains("probe stdout"));
    assert!(String::from_utf8_lossy(&foreground.stderr).contains("probe stderr"));
    assert!(!sandbox.paths.log("api").exists());
    assert_eq!(sandbox.pids("api").len(), 1);
    sandbox.ok(&["start"]);
    wait_for(|| (sandbox.pids("api").len() >= 3).then_some(()));
    wait_for(|| (sandbox.state("api") == ServiceState::Backoff).then_some(()));
    sandbox.ok(&["stop"]);
    let attempts = sandbox.pids("api").len();
    thread::sleep(Duration::from_millis(1200));
    assert_eq!(sandbox.pids("api").len(), attempts);
    assert_eq!(sandbox.state("api"), ServiceState::Stopped);
}

#[test]
fn enabled_services_start_after_daemon_restart_and_disable_now_stops_them() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "long", "on-failure", &sandbox.project);
    // OS 登録の生成は core テストで検証し、ここでは実機のログイン設定を変更しない。
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let target = Target {
        name: Some("api".into()),
        cwd: sandbox.project.clone(),
        all: false,
    };
    runtime
        .block_on(ipc::request(
            &sandbox.paths,
            ipc::Command::Action {
                action: Action::Enable,
                target: target.clone(),
                now: true,
            },
        ))
        .unwrap();
    let first = sandbox.pid("api");
    sandbox.ok(&["daemon", "stop"]);
    assert!(!is_alive(first));
    sandbox.ok(&["daemon", "start"]);
    wait_for(|| (sandbox.state("api") == ServiceState::Running).then_some(()));
    assert_ne!(sandbox.pid("api"), first);
    sandbox.ok(&["disable", "--now"]);
    assert_eq!(sandbox.state("api"), ServiceState::Stopped);
    assert!(!config::load_named(&sandbox.paths, "api").unwrap().enabled);
    sandbox.ok(&["daemon", "stop"]);
    sandbox.ok(&["daemon", "start"]);
    assert_eq!(sandbox.state("api"), ServiceState::Stopped);
}

#[cfg(unix)]
struct ForegroundGuard(Child);

#[cfg(unix)]
impl Drop for ForegroundGuard {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|status| status.is_none()) {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGINT);
            }
        }
        let _ = self.0.wait();
    }
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_foreground_tree_but_log_follow_does_not_stop_background_service() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "tree", "always", &sandbox.project);
    let foreground = sandbox
        .command(&sandbox.project)
        .arg("run")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut foreground = ForegroundGuard(foreground);
    let pids = sandbox.wait_pids("api", 3);
    assert_eq!(sandbox.state("api"), ServiceState::Foreground);
    sandbox.error(&["start"], "SERVICE_RUNNING");
    assert_eq!(
        unsafe { libc::kill(foreground.0.id() as i32, libc::SIGINT) },
        0
    );
    assert_eq!(foreground.0.wait().unwrap().code(), Some(130));
    assert!(pids.iter().all(|pid| !is_alive(*pid)));
    sandbox.ok(&["start"]);
    let pid = sandbox.pid("api");
    let follow = sandbox
        .command(&sandbox.project)
        .args(["logs", "-f"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut follow = ForegroundGuard(follow);
    thread::sleep(Duration::from_millis(250));
    assert_eq!(unsafe { libc::kill(follow.0.id() as i32, libc::SIGINT) }, 0);
    assert!(follow.0.wait().unwrap().success());
    assert_eq!(sandbox.pid("api"), pid);
    assert!(is_alive(pid));
    sandbox.ok(&["stop"]);
}

#[test]
fn doctor_reports_broken_configs_missing_paths_and_env_files_without_secrets() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "once", "never", &sandbox.project);
    let mut config = config::load_named(&sandbox.paths, "api").unwrap();
    config.cwd = sandbox.temp.path().join("missing-cwd");
    config.resolved_executable = sandbox.temp.path().join("missing-program");
    config.env_file = Some(sandbox.temp.path().join("missing-env"));
    config::save(&sandbox.paths, &config).unwrap();
    fs::write(
        sandbox.paths.config("broken"),
        "API_KEY = 'sensitive-missing-quote\n",
    )
    .unwrap();
    let output = sandbox.output(&["doctor", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("sensitive-missing-quote"));
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    for name in ["working-directory", "executable", "env-file", "config"] {
        assert!(
            value["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["check"] == name && check["level"] == "error")
        );
    }
}

#[cfg(unix)]
#[test]
fn large_output_rotates_without_stopping_the_service_or_losing_followed_records() {
    let sandbox = Sandbox::new();
    sandbox.add("api", "flood", "never", &sandbox.project);
    let gate = sandbox.temp.path().join("flood-gate");
    let mut config = config::load_named(&sandbox.paths, "api").unwrap();
    config
        .environment
        .insert("SVCNEST_TEST_GATE".into(), gate.to_str().unwrap().into());
    config::save(&sandbox.paths, &config).unwrap();
    sandbox.ok(&["start"]);
    let pid = sandbox.pid("api");
    let captured = sandbox.temp.path().join("followed.log");
    let output = fs::File::create(&captured).unwrap();
    let mut follow = ForegroundGuard(
        sandbox
            .command(&sandbox.project)
            .args(["logs", "-f", "-n", "0"])
            .stdout(output)
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| {
        fs::read_to_string(&captured)
            .ok()
            .filter(|text| text.contains("flood waiting"))
    });
    fs::write(gate, "go").unwrap();
    wait_for(|| {
        fs::read_to_string(&captured)
            .ok()
            .filter(|text| text.contains("FLOOD-END"))
    });
    assert_eq!(sandbox.pid("api"), pid);
    assert_eq!(sandbox.state("api"), ServiceState::Running);
    assert!(svcnest::logging::generation(&sandbox.paths.log("api"), 2).is_file());
    for number in 0..svcnest::logging::LOG_GENERATIONS {
        let path = if number == 0 {
            sandbox.paths.log("api")
        } else {
            svcnest::logging::generation(&sandbox.paths.log("api"), number)
        };
        if path.exists() {
            assert!(fs::metadata(path).unwrap().len() <= svcnest::logging::MAX_LOG_BYTES);
        }
    }
    unsafe {
        libc::kill(follow.0.id() as i32, libc::SIGINT);
    }
    assert!(follow.0.wait().unwrap().success());
    let text = fs::read_to_string(captured).unwrap();
    let mut counts = [0u32; 800];
    for line in text.lines() {
        if let Some((_, record)) = line.split_once("flood-record-") {
            let index: usize = record.split_once(' ').unwrap().0.parse().unwrap();
            counts[index] += 1;
        }
    }
    for (index, count) in counts.into_iter().enumerate() {
        assert_eq!(count, 1, "record {index} was lost or duplicated");
    }
    assert!(sandbox.ok(&["logs", "-n", "100"]).contains("FLOOD-END"));
    sandbox.ok(&["stop"]);
}

#[cfg(unix)]
#[test]
fn foreground_process_can_read_from_the_controlling_terminal() {
    use std::{
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::process::CommandExt,
        },
    };
    let sandbox = Sandbox::new();
    sandbox.add("api", "interactive", "never", &sandbox.project);
    let (mut master_fd, mut slave_fd) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
    unsafe {
        let mut attributes = std::mem::zeroed();
        assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &mut attributes), 0);
        attributes.c_lflag |= libc::ISIG | libc::ICANON | libc::ECHO;
        attributes.c_cc[libc::VINTR] = 3;
        assert_eq!(
            libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attributes),
            0
        );
        libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        let flags = libc::fcntl(master.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut command = sandbox.command(&sandbox.project);
    command
        .arg("run")
        .stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap());
    unsafe {
        command.pre_exec(|| {
            #[cfg(target_os = "macos")]
            let request = libc::TIOCSCTTY as libc::c_ulong;
            #[cfg(target_os = "linux")]
            let request = libc::TIOCSCTTY;
            if libc::setsid() < 0 || libc::ioctl(libc::STDIN_FILENO, request, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::tcsetpgrp(libc::STDIN_FILENO, libc::getpgrp()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = ForegroundGuard(command.spawn().unwrap());
    drop(command);
    drop(slave);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sandbox.wait_pids("api", 1);
        master.write_all(b"hello\n").unwrap();
        let mut output = Vec::new();
        wait_for(|| {
            let mut buffer = [0u8; 8192];
            match master.read(&mut buffer) {
                Ok(count) => output.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
                Err(error) => panic!("PTY read failed: {error}"),
            }
            String::from_utf8_lossy(&output)
                .contains("stdin received: hello")
                .then_some(())
        });
        master.write_all(&[3]).unwrap();
        let status = wait_for(|| {
            // macOS の端末 close は出力の drain を待つため、終了待ちの間も読み続ける。
            let mut buffer = [0u8; 8192];
            let _ = master.read(&mut buffer);
            child.0.try_wait().unwrap()
        });
        assert_eq!(status.code(), Some(130));
        assert_eq!(sandbox.state("api"), ServiceState::Stopped);
    }));
    drop(master);
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
