//! Black-box native parsing, output and isolated local-state contracts.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};
fn cli(root: &Path, args: &[&str]) -> Output {
    cli_command(root, args).output().unwrap()
}
fn cli_command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_gridstator"));
    command
        .args(args)
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_CACHE_HOME", root.join("cache"));
    for key in [
        "GRIDSTATOR_CONFIG",
        "GRIDSTATOR_CONTROLLER_DATA",
        "GRIDSTATOR_AGENT_DATA",
        "GRIDSTATOR_URL",
        "GRIDSTATOR_TOKEN",
        "GRIDSTATOR_CA_FILE",
    ] {
        command.env_remove(key);
    }
    command
}

#[cfg(unix)]
#[test]
fn custom_absolute_and_relative_config_paths_preserve_parent_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let absolute_parent = root.join("shared project");
    fs::create_dir(&absolute_parent).unwrap();
    for (index, parent, config) in [
        (
            0,
            absolute_parent.clone(),
            absolute_parent.join("client.json"),
        ),
        (
            1,
            root.to_path_buf(),
            Path::new("relative-client.json").to_path_buf(),
        ),
    ] {
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o775)).unwrap();
        let data = root.join(format!("controller-{index}"));
        let output = cli_command(
            root,
            &["controller", "init", "--host", "127.0.0.1", "--port", "0"],
        )
        .env("GRIDSTATOR_CONFIG", &config)
        .env("GRIDSTATOR_CONTROLLER_DATA", &data)
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::metadata(&parent).unwrap().permissions().mode() & 0o7777,
            0o775
        );
        assert_eq!(
            fs::metadata(root.join(&config))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
#[test]
fn every_native_help_command_parses_and_removed_node_runtime_flag_is_absent() {
    let reference: Value =
        serde_json::from_str(include_str!("../contracts/cli-help.json")).unwrap();
    let temp = tempfile::tempdir().unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let mut args = case["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|part| part.as_str().unwrap())
            .collect::<Vec<_>>();
        args.push("--help");
        let result = cli(temp.path(), &args);
        assert!(
            result.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!result.stdout.is_empty());
    }
    let help = cli(temp.path(), &["agents", "set", "--help"]);
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(!help.contains("--node"));
    assert!(help.contains("--act"));
}
#[test]
fn parser_errors_use_exit_two_and_stdout_stderr_are_separate() {
    let temporary = tempfile::tempdir().unwrap();
    for args in [
        vec!["__unknown_command__"],
        vec!["run", "--__unknown_flag__"],
        vec!["login"],
    ] {
        let result = cli(temporary.path(), &args);
        assert_eq!(
            result.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stdout.is_empty());
        assert!(!result.stderr.is_empty());
    }
    let version = cli(temporary.path(), &["--version"]);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
    assert!(version.stderr.is_empty());
}
#[test]
fn snapshot_dry_run_and_discover_need_no_login_and_preserve_sentinels() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    fs::write(root.join("file.txt"), "uncommitted").unwrap();
    fs::write(
        root.join("gridstator.yml"),
        "tasks:\n  test:\n    command: echo hello\n",
    )
    .unwrap();
    let old = root.join("data/gridstator/controller/sentinel");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, "keep data").unwrap();
    let snapshot = cli(root, &["snapshot", "--dry-run", "--json"]);
    assert!(
        snapshot.status.success(),
        "{}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    assert!(snapshot.stderr.is_empty());
    let snapshot: Value = serde_json::from_slice(&snapshot.stdout).unwrap();
    assert_eq!(snapshot["uploaded"], false);
    assert!(snapshot["id"].as_str().unwrap().starts_with("sha256:"));
    assert!(
        snapshot["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "file.txt")
    );
    let found = cli(root, &["discover", "--json"]);
    assert!(
        found.status.success(),
        "{}",
        String::from_utf8_lossy(&found.stderr)
    );
    let found: Value = serde_json::from_slice(&found.stdout).unwrap();
    assert_eq!(found["tasks"]["test"]["command"], "echo hello");
    assert_eq!(fs::read_to_string(old).unwrap(), "keep data");
    assert!(!root.join("config/gridstator/client.json").exists());
}
#[test]
fn build_identity_and_bridge_paths_are_machine_readable() {
    let temporary = tempfile::tempdir().unwrap();
    let result = cli(temporary.path(), &["build-info", "--json"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let info: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
    assert!(info["buildId"].is_string());
    assert_eq!(info["sha256"].as_str().unwrap().len(), 64);
    assert!(info["target"].as_str().unwrap().contains('/'));
    let mut child = Command::new(env!("CARGO_BIN_EXE_gridstator"))
        .args(["bridge"])
        .current_dir(temporary.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            serde_json::to_string(&json!({"version":1,"operation":"paths"}))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let paths: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(
        paths["configPath"]
            .as_str()
            .unwrap()
            .contains("gridstator-v2")
    );
}
#[test]
fn controller_init_no_login_keeps_client_configuration_untouched() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let config = root.join("config/gridstator-v2/client.json");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "existing credentials sentinel").unwrap();
    let data = root.join("controller");
    let initialized = cli(
        root,
        &[
            "controller",
            "init",
            "--data-dir",
            data.to_str().unwrap(),
            "--host",
            "127.0.0.1",
            "--no-login",
        ],
    );
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    assert!(initialized.stdout.is_empty());
    assert_eq!(
        fs::read_to_string(config).unwrap(),
        "existing credentials sentinel"
    );
    assert!(data.join("controller.db").exists());
}
#[tokio::test]
async fn local_login_is_shared_and_revoked_access_needs_explicit_restore() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let directory = root.join("controller");
    let init = cli(
        root,
        &[
            "controller",
            "init",
            "--data-dir",
            directory.to_str().unwrap(),
            "--host",
            "127.0.0.1",
            "--no-login",
        ],
    );
    assert!(init.status.success());
    let args = [
        "controller",
        "local",
        "ensure-login",
        "--data-dir",
        directory.to_str().unwrap(),
        "--json",
    ];
    std::thread::scope(|scope| {
        let first = scope.spawn(|| cli(root, &args));
        let second = scope.spawn(|| cli(root, &args));
        for result in [first.join().unwrap(), second.join().unwrap()] {
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let result: Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(result["status"], "connected");
            assert!(result.get("token").is_none());
        }
    });
    let database = gridstator::db::Db::open(&directory.join("controller.db")).unwrap();
    let tokens = database
        .call(|connection| gridstator::db::list(connection, "tokens"))
        .await
        .unwrap();
    assert_eq!(tokens.len(), 2);
    let saved_path = saved_config_path(root);
    let mut saved: Value = serde_json::from_slice(&fs::read(&saved_path).unwrap()).unwrap();
    saved["url"] = json!("https://192.168.1.77:7300");
    gridstator::state::write_json(&saved_path, &saved).unwrap();
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(
        saved["token"].as_str().unwrap().as_bytes(),
    ));
    database
        .write(move |connection| {
            for mut token in gridstator::db::list(connection, "tokens")? {
                if token["hash"] == hash {
                    token["revokedAt"] = json!(gridstator::state::now());
                    gridstator::db::put(
                        connection,
                        "tokens",
                        token["id"].as_str().unwrap(),
                        &token,
                    )?;
                }
            }
            Ok(())
        })
        .await
        .unwrap();
    let denied = cli(root, &args);
    assert!(denied.status.success());
    let denied: Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert_eq!(denied["status"], "revoked");
    assert_eq!(
        database
            .call(|connection| gridstator::db::list(connection, "tokens"))
            .await
            .unwrap()
            .len(),
        2
    );
    let restored = cli(
        root,
        &[
            "controller",
            "local",
            "ensure-login",
            "--data-dir",
            directory.to_str().unwrap(),
            "--restore",
            "--json",
        ],
    );
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let restored: Value = serde_json::from_slice(&restored.stdout).unwrap();
    assert_eq!(restored["status"], "connected");
    assert!(restored.get("token").is_none());
    assert_eq!(restored["url"], "https://192.168.1.77:7300");
    let saved: Value = serde_json::from_slice(&fs::read(&saved_path).unwrap()).unwrap();
    assert_eq!(saved["url"], "https://192.168.1.77:7300");
    assert_eq!(
        database
            .call(|connection| gridstator::db::list(connection, "tokens"))
            .await
            .unwrap()
            .len(),
        3
    );
}

fn saved_config_path(root: &Path) -> std::path::PathBuf {
    root.join(if cfg!(target_os = "macos") {
        "Library/Application Support/gridstator-v2/client.json"
    } else {
        "config/gridstator-v2/client.json"
    })
}
#[tokio::test]
async fn local_operator_promotion_preserves_the_saved_public_cluster_url() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let directory = root.join("controller");
    assert!(
        cli(
            root,
            &[
                "controller",
                "init",
                "--data-dir",
                directory.to_str().unwrap(),
                "--no-login"
            ]
        )
        .status
        .success()
    );
    let database = gridstator::db::Db::open(&directory.join("controller.db")).unwrap();
    let issued = database
        .write(|connection| {
            gridstator::controller::issue_token(
                connection, "client", "operator", "operator", None, None, None,
            )
        })
        .await
        .unwrap();
    let ca = fs::read_to_string(directory.join("pki/ca.pem")).unwrap();
    let public_url = "https://192.168.1.77:7300";
    let config_path = saved_config_path(root);
    gridstator::state::write_json(&config_path,&json!({"version":1,"url":public_url,"caPem":ca,"caFingerprint":gridstator::pki::fingerprint(&ca).unwrap(),"token":issued["token"]})).unwrap();
    let result = cli(
        root,
        &[
            "controller",
            "local",
            "ensure-login",
            "--data-dir",
            directory.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["status"], "connected");
    let saved: Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    assert_eq!(saved["url"], public_url);
    assert_ne!(saved["token"], issued["token"]);
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(
        saved["token"].as_str().unwrap().as_bytes(),
    ));
    let promoted = database
        .call(move |connection| {
            Ok(gridstator::db::list(connection, "tokens")?
                .into_iter()
                .find(|token| token["hash"] == hash))
        })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(promoted["role"], "admin");
    let status = cli(
        root,
        &[
            "controller",
            "local",
            "status",
            "--data-dir",
            directory.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["url"], public_url);
    assert!(status.get("token").is_none());
}
#[test]
fn local_bootstrap_preserves_an_explicit_foreign_cluster_connection() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let directory = root.join("controller");
    let foreign = root.join("foreign");
    for directory in [&directory, &foreign] {
        assert!(
            cli(
                root,
                &[
                    "controller",
                    "init",
                    "--data-dir",
                    directory.to_str().unwrap(),
                    "--no-login"
                ]
            )
            .status
            .success()
        );
    }
    let ca = fs::read_to_string(foreign.join("pki/ca.pem")).unwrap();
    let config_path = saved_config_path(root);
    gridstator::state::write_json(&config_path,&json!({"version":1,"url":"https://other:7300","caPem":ca,"caFingerprint":gridstator::pki::fingerprint(&ca).unwrap(),"token":"foreign-token-must-be-preserved"})).unwrap();
    let before = fs::read(&config_path).unwrap();
    let result = cli(
        root,
        &[
            "controller",
            "local",
            "ensure-login",
            "--data-dir",
            directory.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(result.status.success());
    let result: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["status"], "differentCluster");
    assert_eq!(fs::read(config_path).unwrap(), before);
    assert!(
        !result
            .to_string()
            .contains("foreign-token-must-be-preserved")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn frozen_local_service_inspection_ownership_and_invalid_configuration_are_read_only() {
    use std::os::unix::fs::PermissionsExt;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let directory = root.join("controller");
    assert!(
        cli(
            root,
            &[
                "controller",
                "init",
                "--data-dir",
                directory.to_str().unwrap(),
                "--no-login"
            ]
        )
        .status
        .success()
    );
    let config = directory.join("controller.json");
    let before = fs::read(&config).unwrap();
    let tools = root.join("fake-service-tools");
    fs::create_dir(&tools).unwrap();
    let systemctl = tools.join("systemctl");
    fs::write(&systemctl,"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$SERVICE_TRACE\"\ncase \" $* \" in *' show '*) /bin/cat \"$SERVICE_PROPERTIES\" ;; *) exit 97 ;; esac\n").unwrap();
    fs::set_permissions(systemctl, fs::Permissions::from_mode(0o755)).unwrap();
    let properties = root.join("service-properties");
    let trace = root.join("service-trace");
    let executable = env!("CARGO_BIN_EXE_gridstator");
    let definition = |arguments: &str, environment: &str| {
        format!(
            "LoadState=loaded\nActiveState=inactive\nExecStart={{ path={executable} ; argv[]={arguments} ; ignore_errors=no ; }}\nEnvironment={environment}\n"
        )
    };
    let invoke = |arguments: &[&str]| {
        cli_command(root, arguments)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    tools.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("SERVICE_TRACE", &trace)
            .env("SERVICE_PROPERTIES", &properties)
            .output()
            .unwrap()
    };
    let status_args = [
        "controller",
        "local",
        "status",
        "--data-dir",
        directory.to_str().unwrap(),
        "--json",
    ];
    let status = |text: &str| {
        fs::write(&properties, text).unwrap();
        let output = invoke(&status_args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let matching = definition(
        &format!(
            "{executable} controller start --data-dir {}",
            directory.display()
        ),
        "",
    );
    let view = status(&matching);
    assert_eq!(view["available"], true);
    assert_eq!(view["service"]["installed"], true);
    assert_eq!(view["service"]["state"], "inactive");
    assert_eq!(view["service"]["canManage"], true);
    assert!(view["clusterId"].is_string());
    assert!(
        view["caFingerprint"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(fs::read_to_string(&trace).unwrap().lines().count(), 1);
    let wrong = definition(
        &format!(
            "{executable} controller start --data-dir {}",
            root.join("another-controller").display()
        ),
        "",
    );
    let view = status(&wrong);
    assert_eq!(view["service"]["canManage"], false);
    assert!(
        view["service"]["reason"]
            .as_str()
            .unwrap()
            .contains("another data directory")
    );
    for arguments in [
        vec![
            "controller",
            "local",
            "service",
            "restart",
            "--data-dir",
            directory.to_str().unwrap(),
        ],
        vec![
            "controller",
            "local",
            "configure",
            "--data-dir",
            directory.to_str().unwrap(),
            "--host",
            "127.0.0.1",
            "--port",
            "7400",
            "--url",
            "https://127.0.0.1:7400",
        ],
    ] {
        let output = invoke(&arguments);
        assert_eq!(output.status.code(), Some(4));
        assert!(String::from_utf8_lossy(&output.stderr).contains("another data directory"));
        assert_eq!(fs::read(&config).unwrap(), before);
    }
    let view = status(&definition(
        &format!("{executable} controller start --with-agent"),
        "GRIDSTATOR_LOG_FORMAT=json",
    ));
    assert_eq!(view["service"]["canManage"], false);
    assert!(
        view["service"]["reason"]
            .as_str()
            .unwrap()
            .contains("another data directory")
    );
    let view = status(&definition(
        &format!("{executable} controller start"),
        &format!(
            "GRIDSTATOR_LOG_FORMAT=json {}",
            serde_json::to_string(&format!(
                "GRIDSTATOR_CONTROLLER_DATA={}",
                directory.display()
            ))
            .unwrap()
        ),
    ));
    assert_eq!(view["service"]["canManage"], true);
    assert!(view["service"]["reason"].is_null());
    let view = status(&definition("/bin/bash -c custom-wrapper", ""));
    assert_eq!(view["service"]["canManage"], false);
    assert!(
        view["service"]["reason"]
            .as_str()
            .unwrap()
            .contains("Cannot verify")
    );
    fs::write(&properties, &matching).unwrap();
    let output = invoke(&[
        "controller",
        "local",
        "configure",
        "--data-dir",
        directory.to_str().unwrap(),
        "--host",
        "127.0.0.1",
        "--port",
        "7400",
        "--url",
        "https://127.0.0.1:7500",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("configured listening port"));
    assert_eq!(fs::read(&config).unwrap(), before);
    let output = invoke(&[
        "controller",
        "local",
        "service",
        "start",
        "--data-dir",
        root.join("missing").to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&output.stderr).contains("does not own"));
    assert!(
        fs::read_to_string(trace)
            .unwrap()
            .lines()
            .all(|line| line.split_whitespace().any(|argument| argument == "show")),
        "inspection and rejection must never invoke service controls"
    );
}
