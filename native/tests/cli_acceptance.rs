//! Observable native CLI help contracts.
use serde_json::Value;
use std::process::{Command, Output};
fn invoke(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gridstator"))
        .args(args)
        .env("GRIDSTATOR_CONFIG", "/TEST_CONFIG/client.json")
        .env("GRIDSTATOR_CONTROLLER_DATA", "/TEST_DATA/controller")
        .env("GRIDSTATOR_AGENT_DATA", "/TEST_DATA/agent")
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
    serde_json::from_str(include_str!("../contracts/cli-help.json")).unwrap()
}
#[test]
fn all_help_paths_match_reviewed_native_bytes() {
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
