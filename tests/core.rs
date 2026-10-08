use clap::Parser;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use svcnest::{
    cli::{Cli, CliCommand},
    config::{self, RestartPolicy, ServiceConfig},
    error::ServiceError,
    ipc::{self, RuntimeStatus},
    logging::{self, RotatingLog},
    paths::{Lock, Paths, atomic_write},
    platform, resolve,
    runner::policy::{Backoff, should_restart},
};

struct TestPaths(Paths);

impl TestPaths {
    fn discover(home: Option<PathBuf>) -> anyhow::Result<Self> {
        Paths::discover(home).map(Self)
    }
}

impl std::ops::Deref for TestPaths {
    type Target = Paths;
    fn deref(&self) -> &Paths {
        &self.0
    }
}

impl Drop for TestPaths {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.runtime);
    }
}

fn config_at(name: &str, cwd: &Path) -> ServiceConfig {
    ServiceConfig {
        version: 1,
        name: name.into(),
        description: String::new(),
        cwd: fs::canonicalize(cwd).unwrap(),
        command: vec!["program".into(), "argument with spaces".into()],
        resolved_executable: std::env::current_exe().unwrap(),
        resolved_script: None,
        enabled: false,
        restart: RestartPolicy::OnFailure,
        stop_timeout_ms: 10_000,
        env_file: None,
        environment: BTreeMap::new(),
    }
}

#[test]
fn name_validation_matches_the_spec_including_windows_reserved_words() {
    for name in [
        "a",
        "0",
        "api.worker_1-test",
        "con",
        "nul",
        "com1.txt",
        &"a".repeat(64),
    ] {
        config::validate_name(name).unwrap();
    }
    for name in [
        "",
        ".hidden",
        "-api",
        "API",
        "with space",
        "../api",
        "サービス",
        &"a".repeat(65),
    ] {
        assert_eq!(
            ServiceError::code(&config::validate_name(name).unwrap_err()),
            "INVALID_NAME"
        );
    }
}

#[test]
fn invalid_configs_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let valid = config_at("api", temp.path());
    valid.validate().unwrap();
    let cases: Vec<fn(&mut ServiceConfig)> = vec![
        |c| c.version = 2,
        |c| c.cwd = "relative".into(),
        |c| c.resolved_executable = "relative".into(),
        |c| c.resolved_script = Some("relative.js".into()),
        |c| c.env_file = Some(".env".into()),
        |c| c.command.clear(),
        |c| c.command.push("bad\0arg".into()),
        |c| c.stop_timeout_ms = 0,
        |c| c.stop_timeout_ms = 300_001,
        |c| {
            c.environment.insert("BAD=KEY".into(), "value".into());
        },
        |c| {
            c.environment.insert("KEY".into(), "bad\0value".into());
        },
    ];
    for mutate in cases {
        let mut config = valid.clone();
        mutate(&mut config);
        assert!(config.validate().is_err());
    }
}

#[test]
fn config_roundtrip_and_redaction_do_not_load_secrets_from_env_file() {
    let temp = tempfile::tempdir().unwrap();
    let paths = TestPaths::discover(Some(temp.path().join("home"))).unwrap();
    let mut config = config_at("con", temp.path());
    config
        .environment
        .insert("PATH".into(), "registration-path".into());
    config
        .environment
        .insert("API_KEY".into(), "explicit-secret".into());
    let env_file = temp.path().join(".env");
    fs::write(
        &env_file,
        "FROM_FILE=file-secret\nAPI_KEY=file-value\nPATH=file-path\n",
    )
    .unwrap();
    config.env_file = Some(fs::canonicalize(env_file).unwrap());
    config::save(&paths, &config).unwrap();
    let loaded = config::load_named(&paths, "con").unwrap();
    let environment = loaded.effective_environment().unwrap();
    assert_eq!(environment["API_KEY"], "explicit-secret");
    assert_eq!(environment["FROM_FILE"], "file-secret");
    assert_eq!(environment["PATH"], "registration-path");
    let saved = fs::read_to_string(paths.config("con")).unwrap();
    assert!(!saved.contains("file-secret"));
    let masked = toml::to_string(&loaded.redacted()).unwrap();
    assert!(!masked.contains("explicit-secret"));
    assert!(masked.contains("registration-path"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(paths.config("con"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn malformed_config_diagnostics_never_echo_input_values() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("svc-api.toml");
    fs::write(&file, "API_KEY = 'sensitive-value\n").unwrap();
    let error = format!("{:#}", config::load(&file).unwrap_err());
    assert!(!error.contains("sensitive-value"));
}

#[test]
fn config_filename_cannot_impersonate_another_service() {
    let temp = tempfile::tempdir().unwrap();
    let paths = TestPaths::discover(Some(temp.path().join("home"))).unwrap();
    let config = config_at("api", temp.path());
    config::save(&paths, &config).unwrap();
    fs::rename(paths.config("api"), paths.config("worker")).unwrap();
    assert_eq!(
        ServiceError::code(&config::load_named(&paths, "worker").unwrap_err()),
        "INVALID_CONFIG"
    );
}

#[test]
fn atomic_updates_are_always_readable() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("data.json");
    atomic_write(&path, br#"{"generation":0}"#).unwrap();
    let reader_path = path.clone();
    let reader = std::thread::spawn(move || {
        for _ in 0..2000 {
            let bytes = loop {
                match fs::read(&reader_path) {
                    Ok(bytes) => break bytes,
                    // Windows may briefly reject a reader while ReplaceFileW is
                    // committing the replacement metadata. Retry the open; a
                    // successful read must still contain a complete JSON generation.
                    #[cfg(windows)]
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("cannot read atomic file: {error}"),
                }
            };
            let _: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        }
    });
    for index in 1..60 {
        atomic_write(&path, format!("{{\"generation\":{index}}}").as_bytes()).unwrap();
    }
    reader.join().unwrap();
}

#[test]
fn a_lock_is_exclusive_and_released_on_drop() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("daemon.lock");
    let lock = Lock::try_acquire(&path).unwrap().unwrap();
    assert!(Lock::try_acquire(&path).unwrap().is_none());
    drop(lock);
    assert!(Lock::try_acquire(&path).unwrap().is_some());
}

#[test]
fn executable_resolution_handles_path_relative_absolute_and_missing() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let file = bin.join(if cfg!(windows) { "probe.EXE" } else { "probe" });
    fs::write(&file, b"test executable").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let absolute = fs::canonicalize(&file).unwrap();
    let path = std::env::join_paths([&bin]).unwrap();
    assert_eq!(
        resolve::executable::resolve(
            "probe",
            temp.path(),
            Some(&path),
            Some(OsStr::new(".COM;.EXE"))
        )
        .unwrap(),
        absolute
    );
    assert_eq!(
        resolve::executable::resolve(
            if cfg!(windows) {
                "./bin/probe.EXE"
            } else {
                "./bin/probe"
            },
            temp.path(),
            None,
            None
        )
        .unwrap(),
        absolute
    );
    assert_eq!(
        resolve::executable::resolve(absolute.to_str().unwrap(), temp.path(), None, None).unwrap(),
        absolute
    );
    assert_eq!(
        ServiceError::code(
            &resolve::executable::resolve("missing", temp.path(), Some(&path), None).unwrap_err()
        ),
        "EXECUTABLE_NOT_FOUND"
    );
    assert!(resolve::executable::resolve("bin", temp.path(), Some(&path), None).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(resolve::executable::resolve("probe", temp.path(), Some(&path), None).is_err());
    }
}

#[test]
fn relative_path_entries_are_resolved_against_service_cwd() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let file = bin.join(if cfg!(windows) { "probe.exe" } else { "probe" });
    fs::write(&file, "probe").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(
        resolve::executable::resolve("probe", temp.path(), Some(OsStr::new("bin")), None).unwrap(),
        fs::canonicalize(file).unwrap()
    );
}

#[test]
fn working_directory_is_canonical_and_must_be_a_directory() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("sub")).unwrap();
    assert_eq!(
        resolve::executable::working_directory(Some(&temp.path().join("sub/.."))).unwrap(),
        fs::canonicalize(temp.path()).unwrap()
    );
    fs::write(temp.path().join("file"), "not a directory").unwrap();
    assert!(resolve::executable::working_directory(Some(&temp.path().join("file"))).is_err());
}

#[test]
fn service_resolution_uses_exact_then_nearest_parent_and_explicit_name() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let child = project.join("src/routes");
    fs::create_dir_all(&child).unwrap();
    let configs = vec![config_at("outer", temp.path()), config_at("api", &project)];
    assert_eq!(
        resolve::service::resolve(&configs, None, &project, false, "status").unwrap()[0].name,
        "api"
    );
    assert_eq!(
        resolve::service::resolve(&configs, None, &child, false, "status").unwrap()[0].name,
        "api"
    );
    assert_eq!(
        resolve::service::resolve(
            &configs,
            Some("outer"),
            &child.join("nonexistent"),
            false,
            "status"
        )
        .unwrap()[0]
            .name,
        "outer"
    );
    assert_eq!(
        ServiceError::code(
            &resolve::service::resolve(&configs, Some("missing"), &child, false, "status")
                .unwrap_err()
        ),
        "SERVICE_NOT_FOUND"
    );
    assert!(resolve::service::resolve(&configs, Some("api"), &child, true, "status").is_err());
}

#[test]
fn ambiguity_never_falls_back_to_a_parent_and_all_is_directory_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let child = project.join("src");
    fs::create_dir_all(&child).unwrap();
    let configs = vec![
        config_at("outer", temp.path()),
        config_at("worker", &project),
        config_at("api", &project),
    ];
    let error = resolve::service::resolve(&configs, None, &child, false, "start").unwrap_err();
    assert_eq!(ServiceError::code(&error), "AMBIGUOUS_SERVICE");
    assert!(error.to_string().contains("svcnest start --all"));
    let names = resolve::service::resolve(&configs, None, &child, true, "status")
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["api", "worker"]);
    let run_error = resolve::service::resolve(&configs, None, &child, false, "run").unwrap_err();
    assert!(!run_error.to_string().contains("--all"));
    assert_eq!(
        ServiceError::code(
            &resolve::service::resolve(&[], None, &child, false, "status").unwrap_err()
        ),
        "SERVICE_NOT_FOUND"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_working_directory_resolves_to_the_same_service() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir(&project).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&project, &link).unwrap();
    assert_eq!(
        resolve::service::resolve(&[config_at("api", &project)], None, &link, false, "status")
            .unwrap()[0]
            .name,
        "api"
    );
    assert!(TestPaths::discover(Some(link)).is_err());
}

#[test]
fn restart_policies_never_revive_manual_stops() {
    for policy in [
        RestartPolicy::Never,
        RestartPolicy::OnFailure,
        RestartPolicy::Always,
    ] {
        assert!(!should_restart(policy, false, true));
        assert!(!should_restart(policy, true, true));
    }
    assert!(!should_restart(RestartPolicy::Never, false, false));
    assert!(!should_restart(RestartPolicy::OnFailure, true, false));
    assert!(should_restart(RestartPolicy::OnFailure, false, false));
    assert!(should_restart(RestartPolicy::Always, true, false));
}

#[test]
fn backoff_caps_resets_and_limits_restarts_in_a_rolling_window() {
    let now = Instant::now();
    let mut backoff = Backoff::default();
    for seconds in [1, 2, 4, 8, 16, 30, 30, 30, 30, 30] {
        assert_eq!(
            backoff.next(now, Duration::ZERO),
            Some(Duration::from_secs(seconds))
        );
    }
    assert_eq!(backoff.next(now, Duration::ZERO), None);
    assert_eq!(
        backoff.next(now + Duration::from_secs(300), Duration::from_secs(60)),
        Some(Duration::from_secs(1))
    );
    let mut backoff = Backoff::default();
    backoff.next(now, Duration::ZERO);
    backoff.next(now, Duration::ZERO);
    assert_eq!(
        backoff.next(now, Duration::from_secs(60)),
        Some(Duration::from_secs(1))
    );
}

#[test]
fn rotation_keeps_five_generations_and_tail_crosses_files() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("service.log");
    let mut log = RotatingLog::with_limits(&path, 100, 5).unwrap();
    for index in 0..8 {
        log.record(
            "stdout",
            format!("line-{index} {}", "x".repeat(40)).as_bytes(),
        )
        .unwrap();
    }
    drop(log);
    assert!(!logging::generation(&path, 5).exists());
    assert!(logging::generation(&path, 4).exists());
    let tail = logging::tail(&path, 3).unwrap();
    assert!(tail.contains("line-5"));
    assert!(tail.contains("line-6"));
    assert!(tail.contains("line-7"));
    assert!(!tail.contains("line-4"));
    assert!(logging::tail(&path, 0).unwrap().is_empty());
}

#[tokio::test]
async fn capture_flushes_partial_lines_and_bounds_large_lines() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("service.log");
    let log = std::sync::Arc::new(std::sync::Mutex::new(RotatingLog::new(&path).unwrap()));
    let data = [vec![b'x'; 70_000], b"\nlast partial line".to_vec()].concat();
    logging::capture(data.as_slice(), "stderr", log)
        .await
        .unwrap();
    let text = fs::read_to_string(path).unwrap();
    assert_eq!(text.lines().count(), 3);
    assert!(text.contains("stderr | last partial line"));
}

#[tokio::test]
async fn ipc_rejects_oversized_truncated_or_malformed_frames() {
    use tokio::io::BufReader;
    for bytes in [
        b"not-json\n".to_vec(),
        b"{\"state\":".to_vec(),
        vec![b'x'; ipc::MAX_FRAME + 1],
    ] {
        assert!(
            ipc::read_frame::<RuntimeStatus>(&mut BufReader::new(bytes.as_slice()))
                .await
                .is_err()
        );
    }
    assert!(
        ipc::read_frame::<RuntimeStatus>(&mut BufReader::new(&b""[..]))
            .await
            .unwrap()
            .is_none()
    );
    let mut bytes = Vec::new();
    ipc::write_frame(&mut bytes, &RuntimeStatus::default())
        .await
        .unwrap();
    assert!(
        ipc::read_frame::<RuntimeStatus>(&mut BufReader::new(bytes.as_slice()))
            .await
            .unwrap()
            .is_some()
    );
}

#[test]
fn cli_requires_an_argv_separator_and_preserves_arguments() {
    let cli = Cli::try_parse_from([
        "svcnest",
        "add",
        "api",
        "--env",
        "TOKEN=a=b",
        "--stop-timeout",
        "500ms",
        "--",
        "program",
        "with spaces",
        "$HOME; echo no",
        "--port",
        "-1",
    ])
    .unwrap();
    let CliCommand::Add(args) = cli.command else {
        panic!("wrong command")
    };
    assert_eq!(
        args.command,
        ["program", "with spaces", "$HOME; echo no", "--port", "-1"]
    );
    assert_eq!(args.env, [("TOKEN".into(), "a=b".into())]);
    assert_eq!(args.stop_timeout, 500);
    assert!(Cli::try_parse_from(["svcnest", "start", "api", "--all"]).is_err());
    assert!(Cli::try_parse_from(["svcnest", "add", "api", "program"]).is_err());
    assert!(
        Cli::try_parse_from([
            "svcnest",
            "add",
            "api",
            "--stop-timeout",
            "0",
            "--",
            "program"
        ])
        .is_err()
    );
}

#[test]
fn os_registration_contains_only_the_daemon_and_escapes_paths() {
    let temp = tempfile::tempdir().unwrap();
    let paths = TestPaths::discover(Some(temp.path().join("home with & spaces"))).unwrap();
    let executable = temp.path().join("svcnest with spaces");
    let text = platform::render(&paths, &executable).unwrap();
    assert!(text.contains("daemon"));
    assert!(text.contains("serve"));
    assert!(!text.contains("target process"));
    #[cfg(target_os = "macos")]
    {
        assert!(text.contains("<array>"));
        assert!(text.contains("home with &amp; spaces"));
    }
    #[cfg(target_os = "linux")]
    {
        assert!(text.contains("ExecStart=\""));
        assert!(text.contains("WantedBy=default.target"));
        assert_eq!(
            svcnest::platform::linux::quote("%$\"\\"),
            "\"%%$$\\\"\\\\\""
        );
    }
    #[cfg(windows)]
    {
        assert!(text.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(text.contains("<RunLevel>LeastPrivilege</RunLevel>"));
        assert!(!text.contains("HighestAvailable"));
        assert_eq!(
            svcnest::platform::windows::quote_arg("a\\\"b\\"),
            "\"a\\\\\\\"b\\\\\""
        );
    }
}
