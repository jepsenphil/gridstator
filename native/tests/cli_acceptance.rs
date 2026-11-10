//! Exact observable CLI contracts from the frozen Commander implementation.
use serde_json::Value;
use std::process::{Command, Output};
fn invoke(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gridstator"))
        .args(args)
        .env("GRIDSTATOR_CONFIG", "/V1_CONFIG/client.json")
        .env("GRIDSTATOR_CONTROLLER_DATA", "/V1_DATA/controller")
        .env("GRIDSTATOR_AGENT_DATA", "/V1_DATA/agent")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env_remove("FORCE_COLOR")
        .output()
        .unwrap()
}
fn normalize(bytes: Vec<u8>) -> String {
    let text = String::from_utf8(bytes).unwrap();
    let hostname = sysinfo::System::host_name().unwrap_or_else(|| "node".into());
    let hostname = hostname
        .split('.')
        .next()
        .unwrap_or("node")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let hostname = hostname.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    let hostname = if hostname.is_empty() {
        "node"
    } else {
        hostname
    };
    let quoted = serde_json::to_string(hostname).unwrap();
    let real = regex::Regex::new(&format!(r"\(default:\s+{}\)", regex::escape(&quoted))).unwrap();
    let canonical = regex::Regex::new(r#"\(default:\s+"<HOSTNAME>"\)"#).unwrap();
    let mut output = String::new();
    let mut block = String::new();
    let mut host_option = false;
    let flush = |output: &mut String, block: &mut String, host_option: bool| {
        if host_option {
            let replaced = real.replace_all(block, "(default: \"<HOSTNAME>\")");
            output.push_str(&canonical.replace_all(&replaced, "(default: \"<HOSTNAME>\")"));
        } else {
            output.push_str(block);
        }
        block.clear();
    };
    for line in text.split_inclusive('\n') {
        if line.starts_with("  -") || line.trim().is_empty() {
            flush(&mut output, &mut block, host_option);
            host_option = line.starts_with("  --agent-name ") || line.starts_with("  --name ");
        }
        block.push_str(line);
    }
    flush(&mut output, &mut block, host_option);
    output
}

fn help() -> Value {
    serde_json::from_str(include_str!("../contracts/cli-native-help-v2.json")).unwrap()
}
#[test]
fn all_fifty_six_help_paths_match_source_commander_bytes() {
    let fixture = help();
    let mut failures = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let mut args = case["path"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        args.push("--help".into());
        let output = invoke(&args);
        let stdout = normalize(output.stdout);
        let stderr = normalize(output.stderr);
        if output.status.code() != Some(0)
            || stdout != normalize(case["stdout"].as_str().unwrap().as_bytes().to_vec())
            || stderr != normalize(case["stderr"].as_str().unwrap().as_bytes().to_vec())
        {
            failures.push(format!(
                "{args:?}: code {:?}; stdout {:?}; stderr {:?}",
                output.status.code(),
                stdout,
                stderr
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} help differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
#[test]
fn all_source_parser_boundaries_preserve_bytes_streams_and_exit_codes() {
    let fixture: Value =
        serde_json::from_str(include_str!("../contracts/cli-process-boundaries-v1.json")).unwrap();
    let original: Value =
        serde_json::from_str(include_str!("../contracts/accepted-v1/cli-help.json")).unwrap();
    let native = help();
    let replace_help = |text: &str| -> String {
        if let Some(case) = original["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["stdout"] == text)
        {
            return native["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|native| native["path"] == case["path"])
                .unwrap()["stdout"]
                .as_str()
                .unwrap()
                .to_owned();
        }
        if text == "0.1.0\n" {
            return format!("{}\n", env!("CARGO_PKG_VERSION"));
        }
        text.to_owned()
    };
    let mut failures = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let args = case["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let output = invoke(&args);
        let stdout = normalize(output.stdout);
        let stderr = normalize(output.stderr);
        let expected_stdout =
            normalize(replace_help(case["stdout"].as_str().unwrap()).into_bytes());
        let expected_stderr =
            normalize(replace_help(case["stderr"].as_str().unwrap()).into_bytes());
        if output.status.code() != case["exitCode"].as_i64().map(|code| code as i32)
            || stdout != expected_stdout
            || stderr != expected_stderr
        {
            failures.push(format!(
                "{args:?}: code {:?}; expected stderr {:?}; actual stderr {:?}; stdout {:?}",
                output.status.code(),
                expected_stderr,
                stderr,
                stdout
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} parser differences:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
#[test]
fn production_registry_retains_the_frozen_workflow_flags_and_arities() {
    let fixture: Value =
        serde_json::from_str(include_str!("../contracts/accepted-v1/cli-help.json")).unwrap();
    let command = gridstator::cli::command();
    for case in fixture["cases"].as_array().unwrap() {
        let mut actual = &command;
        for name in case["path"].as_array().unwrap() {
            actual = actual.find_subcommand(name.as_str().unwrap()).unwrap();
        }
        for option in case["options"].as_array().unwrap() {
            let flags = option["flags"].as_str().unwrap();
            let long = flags
                .split(',')
                .find_map(|part| {
                    part.split_whitespace()
                        .next()
                        .and_then(|flag| flag.strip_prefix("--"))
                })
                .unwrap();
            if case["path"] == serde_json::json!(["agents", "set"]) && long == "node" {
                assert!(
                    actual
                        .get_arguments()
                        .all(|argument| argument.get_long() != Some("node"))
                );
                continue;
            }
            let argument = actual
                .get_arguments()
                .find(|argument| argument.get_long() == Some(long))
                .unwrap_or_else(|| panic!("missing {flags} in {}", case["path"]));
            let short = flags.split(',').find_map(|part| {
                part.split_whitespace()
                    .next()
                    .filter(|flag| flag.starts_with('-') && !flag.starts_with("--"))
                    .and_then(|flag| flag.chars().nth(1))
            });
            assert_eq!(argument.get_short(), short, "{} {flags}", case["path"]);
            assert_eq!(
                argument.get_action().takes_values(),
                flags.contains('<') || flags.contains('['),
                "{} {flags}",
                case["path"]
            );
        }
    }
    let declarations: Value =
        serde_json::from_str(include_str!("../src/cli/commands.json")).unwrap();
    assert!(
        declarations["cases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|case| case.get("stdout").is_none() && case.get("stderr").is_none()),
        "production declarations must not embed expected help output"
    );
}
