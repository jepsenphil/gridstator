use gridstator::services::{
    ServiceAction, ServiceConfig, ServiceManager, ServicePlatform, ServiceRole, ServiceScope,
    render_launchd, render_systemd,
};
use std::path::PathBuf;

fn config(role: ServiceRole) -> ServiceConfig {
    ServiceConfig::new(
        role,
        PathBuf::from("/home/test user/managed store/supervisor"),
        PathBuf::from("/home/test user/managed store"),
        PathBuf::from("/home/test user/native data"),
        PathBuf::from("/home/test user"),
        501,
    )
}

#[test]
fn source_001_controller_user_unit_preserves_payload_arguments_and_deduplicates_path() {
    let mut value = config(ServiceRole::Controller);
    value.extra_args = ["--with-agent", "--agent-name", "my laptop"]
        .map(String::from)
        .to_vec();
    value.path_dirs = ["/home/me/.local/bin", "/usr/bin", "/usr/bin"]
        .map(PathBuf::from)
        .to_vec();
    let output = render_systemd(&value).unwrap();
    assert_eq!(value.unit_name(), "gridstator-native-controller.service");
    assert!(output.contains("\"controller\" \"start\" \"--data-dir\" \"/home/test user/native data\" \"--with-agent\" \"--agent-name\" \"my laptop\""));
    assert!(output.contains("Environment=\"PATH=/home/me/.local/bin:/usr/bin\"\n"));
    assert!(output.contains("WantedBy=default.target"));
    assert!(!output.contains("User="));
}

#[test]
fn source_002_agent_system_unit_keeps_account_docker_access_home_and_data_directory() {
    let mut value = config(ServiceRole::Agent);
    value.scope = ServiceScope::System;
    value.user = Some("pj".into());
    value.group = Some("pj".into());
    value.home = PathBuf::from("/home/pj");
    value.data_dir = PathBuf::from("/home/pj/.local/share/gridstator-v2/agent");
    value.supplementary_groups = vec!["docker".into()];
    let output = render_systemd(&value).unwrap();
    assert!(
        output
            .contains("User=pj\nGroup=pj\nSupplementaryGroups=docker\nWorkingDirectory=/home/pj\n")
    );
    assert!(output.contains(
        "\"agent\" \"start\" \"--data-dir\" \"/home/pj/.local/share/gridstator-v2/agent\""
    ));
    assert!(output.contains("WantedBy=multi-user.target"));
}

#[test]
fn systemd_user_definition_runs_the_native_supervisor_and_escapes_service_arguments() {
    let mut value = config(ServiceRole::Controller);
    value.extra_args = vec![
        "--with-agent".into(),
        "--agent-name".into(),
        "literal $HOME %i \"quote\"".into(),
    ];
    value
        .environment
        .insert("DOCKER_CONTEXT".into(), "desktop-linux".into());
    let output = render_systemd(&value).unwrap();
    assert!(output.contains("\"supervise\" \"--install-root\" \"/home/test user/managed store\" \"--\" \"controller\" \"start\""));
    assert!(output.contains("literal $$HOME %%i \\\"quote\\\""));
    assert!(output.contains("Environment=\"HOME=/home/test user\""));
    assert!(output.contains("Environment=\"DOCKER_CONTEXT=desktop-linux\""));
    assert!(output.contains("/home/test user/.docker/bin"));
    assert!(output.contains("WantedBy=default.target"));
    assert!(output.contains("KillMode=mixed"));
    assert!(output.contains("SuccessExitStatus=130 143"));
    assert!(!output.contains("User="));
    assert!(!output.contains("node "));
    assert_eq!(
        value.definition_path(ServicePlatform::Systemd).unwrap(),
        PathBuf::from("/home/test user/.config/systemd/user/gridstator-native-controller.service")
    );
}

#[test]
fn system_scope_requires_an_explicit_account_and_renders_boot_start_configuration() {
    let mut value = config(ServiceRole::Agent);
    value.scope = ServiceScope::System;
    assert!(render_systemd(&value).is_err());
    value.user = Some("worker".into());
    value.group = Some("worker".into());
    value.supplementary_groups = vec!["docker".into()];
    let output = render_systemd(&value).unwrap();
    assert!(output.contains("User=worker\nGroup=worker\nSupplementaryGroups=docker"));
    assert!(output.contains("WantedBy=multi-user.target"));
    assert_eq!(
        value.definition_path(ServicePlatform::Systemd).unwrap(),
        PathBuf::from("/etc/systemd/system/gridstator-native-agent.service")
    );
    let mac = render_launchd(&value).unwrap();
    assert!(mac.contains("<key>UserName</key><string>worker</string>"));
    assert_eq!(
        value.definition_path(ServicePlatform::Launchd).unwrap(),
        PathBuf::from("/Library/LaunchDaemons/org.gridstator.native.agent.plist")
    );
}

#[test]
fn launchd_uses_argument_array_account_home_docker_context_and_separate_logs() {
    let mut value = config(ServiceRole::Agent);
    value.extra_args = vec!["--name".into(), "a<&\"' name".into()];
    value.environment.insert(
        "DOCKER_HOST".into(),
        "unix:///Users/test/.docker/run/docker.sock".into(),
    );
    let output = render_launchd(&value).unwrap();
    assert!(output.contains("<key>ProgramArguments</key><array>"));
    assert!(output.contains("<string>/home/test user/managed store/supervisor</string>"));
    assert!(output.contains("<string>a&lt;&amp;&quot;&apos; name</string>"));
    assert!(output.contains("<key>HOME</key><string>/home/test user</string>"));
    assert!(output.contains("<key>DOCKER_CONFIG</key><string>/home/test user/.docker</string>"));
    assert!(output.contains("/home/test user/.docker/bin"));
    assert!(output.contains(
        "<key>DOCKER_HOST</key><string>unix:///Users/test/.docker/run/docker.sock</string>"
    ));
    assert!(output.contains("/opt/homebrew/bin"));
    assert!(output.contains("/usr/local/bin"));
    assert!(output.contains("/service-logs/stdout.log"));
    assert!(output.contains("/service-logs/stderr.log"));
    assert!(output.contains("<key>RunAtLoad</key><true/>"));
    assert!(!output.contains("<key>UserName</key>"));
    assert_eq!(
        value.definition_path(ServicePlatform::Launchd).unwrap(),
        PathBuf::from("/home/test user/Library/LaunchAgents/org.gridstator.native.agent.plist")
    );
    // Parse actual XML so an escaping mistake is not hidden by individual string assertions.
    let mut reader = quick_xml::Reader::from_str(&output);
    while reader.read_event().unwrap() != quick_xml::events::Event::Eof {}
}

#[test]
fn controls_are_argument_vectors_and_mac_stop_boots_out_instead_of_triggering_keepalive() {
    let value = config(ServiceRole::Controller);
    let linux = ServiceManager::new(ServicePlatform::Systemd);
    let commands = linux
        .control_commands(&value, ServiceAction::Restart)
        .unwrap();
    assert_eq!(commands[0].program, "systemctl");
    assert_eq!(
        commands[0].args,
        ["--user", "restart", "gridstator-native-controller.service"].map(std::ffi::OsString::from)
    );
    let mac = ServiceManager::new(ServicePlatform::Launchd);
    let commands = mac.control_commands(&value, ServiceAction::Stop).unwrap();
    assert_eq!(commands[0].program, "launchctl");
    assert_eq!(
        commands[0].args,
        ["bootout", "gui/501/org.gridstator.native.controller"].map(std::ffi::OsString::from)
    );
    let commands = mac.control_commands(&value, ServiceAction::Start).unwrap();
    assert_eq!(commands[1].args[0], "bootstrap");
    assert_eq!(commands[1].args[1], "gui/501");
}

#[test]
fn service_definitions_reject_control_injection_relative_paths_and_ambiguous_names() {
    let mut value = config(ServiceRole::Agent);
    value.extra_args.push("name\nExecStart=/evil".into());
    assert!(render_systemd(&value).is_err());
    assert!(render_launchd(&value).is_err());
    value.extra_args.clear();
    value.data_dir = PathBuf::from("relative");
    assert!(render_systemd(&value).is_err());
    value.data_dir = PathBuf::from("/absolute");
    value.name = "../foreign-service".into();
    assert!(value.definition_path(ServicePlatform::Systemd).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn actual_systemd_parser_accepts_working_directory_with_spaces() -> std::io::Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home with spaces %i");
    std::fs::create_dir(&home)?;
    let mut value = ServiceConfig::new(
        ServiceRole::Agent,
        PathBuf::from("/usr/bin/true"),
        temp.path().join("store"),
        temp.path().join("data"),
        home.clone(),
        1000,
    );
    value.name = "gridstator-verify-only".into();
    let text = render_systemd(&value)?;
    assert!(text.contains(&format!(
        "WorkingDirectory={}\n",
        home.to_string_lossy().replace('%', "%%")
    )));
    let unit = temp.path().join(value.unit_name());
    std::fs::write(&unit, text)?;
    let output = match std::process::Command::new("systemd-analyze")
        .arg("verify")
        .arg(&unit)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("systemd-analyze unavailable; platform parser gate remains external");
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    assert!(
        output.status.success(),
        "systemd-analyze rejected rendered definition: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
