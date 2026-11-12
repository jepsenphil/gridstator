//! Native CLI presentation. Reference command inventory is static data; execution stays native.
use crate::{client::Client, discovery, protocol, state, workspace};
use anyhow::{Context, Result, bail, ensure};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value, json};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

mod parse;
mod presentation;
fn catalog() -> &'static [Value] {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut declarations: Value = serde_json::from_str(include_str!("cli/commands.json"))
            .expect("static CLI declarations");
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
        for case in declarations["cases"].as_array_mut().unwrap() {
            for option in case["options"].as_array_mut().unwrap() {
                if let Some(default) = option.get("default").and_then(Value::as_str) {
                    let replacement = match default {
                        "/TEST_DATA/controller" => {
                            Some(state::data_dir("controller").to_string_lossy().into_owned())
                        }
                        "/TEST_DATA/agent" => {
                            Some(state::data_dir("agent").to_string_lossy().into_owned())
                        }
                        "/TEST_CONFIG/client.json" => {
                            Some(state::config_path().to_string_lossy().into_owned())
                        }
                        "<HOSTNAME>" => Some(
                            if hostname.is_empty() {
                                "node"
                            } else {
                                hostname
                            }
                            .to_owned(),
                        ),
                        _ => None,
                    };
                    if let Some(replacement) = replacement {
                        option["default"] = json!(replacement);
                    }
                }
            }
        }
        declarations
    })["cases"]
        .as_array()
        .expect("CLI cases")
}
fn reference_command(case: &'static Value) -> Command {
    let path = case["path"].as_array().unwrap();
    let name = path.last().and_then(Value::as_str).unwrap_or("gridstator");
    let mut command = Command::new(name)
        .about(case["description"].as_str().unwrap_or(""))
        .disable_version_flag(true)
        .disable_help_subcommand(case["implicitHelp"] != true)
        .args_override_self(true);
    if path.is_empty() {
        command = command.version(crate::VERSION).propagate_version(true);
    }
    if path
        .iter()
        .filter_map(Value::as_str)
        .eq(["controller", "publish-build"])
    {
        command = command.about("Publish a verified native release bundle (manifest and four target archives), drain the controller and restart (admin)");
    }
    if path.last().and_then(Value::as_str) == Some("install-service") {
        command = command.about("Install an isolated native service: systemd on Linux, launchd on macOS. User services start at login; --system starts at boot.");
    }
    if path.last().and_then(Value::as_str) == Some("uninstall-service") {
        command = command.about(
            if path.first().and_then(Value::as_str) == Some("controller") {
                "Stop and remove the isolated native controller service (keeps data)"
            } else {
                "Stop and remove the isolated native agent service (keeps data)"
            },
        );
    }
    for option in case["options"].as_array().unwrap() {
        let flags = option["flags"].as_str().unwrap();
        let pieces = flags.split(',').map(str::trim).collect::<Vec<_>>();
        let long = pieces
            .iter()
            .find_map(|piece| {
                piece
                    .split_whitespace()
                    .next()
                    .and_then(|flag| flag.strip_prefix("--"))
            })
            .unwrap();
        if path.iter().filter_map(Value::as_str).eq(["agents", "set"]) && long == "node" {
            continue;
        }
        let mut argument = Arg::new(long)
            .long(long)
            .help(option["description"].as_str().unwrap_or(""));
        if let Some(short) = pieces.iter().find_map(|piece| {
            piece
                .strip_prefix('-')
                .filter(|value| !value.starts_with('-'))
                .and_then(|value| value.chars().next())
        }) {
            argument = argument.short(short);
        }
        if long == "version" {
            argument = argument.action(ArgAction::Version).global(true);
        } else if flags.contains('<') || flags.contains('[') {
            let repeated = option["default"].is_array()
                || option["description"]
                    .as_str()
                    .is_some_and(|description| description.contains("repeatable"));
            argument = argument.action(if repeated {
                ArgAction::Append
            } else {
                ArgAction::Set
            });
            if flags.contains("...>") {
                argument = argument.num_args(1..).action(ArgAction::Append);
            }
            if flags.contains("[<") {
                argument = argument.num_args(0..=1);
            }
        } else {
            argument = argument.action(ArgAction::SetTrue);
        }
        argument = argument.required(option["mandatory"] == true);
        if let Some(default) = option
            .get("default")
            .and_then(Value::as_str)
            .filter(|v| !v.starts_with("/TEST_") && *v != "<HOSTNAME>")
        {
            argument = argument.default_value(default);
        }
        if long == "print" && path.last().and_then(Value::as_str) == Some("install-service") {
            argument = argument.help("print the service definition and exit");
        }
        command = command.arg(argument);
    }
    for (index, parameter) in case["arguments"].as_array().unwrap().iter().enumerate() {
        let name = parameter["name"].as_str().unwrap();
        let required = parameter["required"] == true;
        let mut argument = Arg::new(name)
            .required(required)
            .index(index + 1)
            .help(parameter["description"].as_str().unwrap_or(""));
        if path
            .iter()
            .filter_map(Value::as_str)
            .eq(["controller", "publish-build"])
        {
            argument = argument
                .help("native release bundle (.tar.gz); defaults to GRIDSTATOR_RELEASE_BUNDLE");
        }
        if parameter["variadic"] == true {
            argument = argument
                .num_args(if required { 1.. } else { 0.. })
                .action(ArgAction::Append);
        }
        command = command.arg(argument);
    }
    for child in catalog().iter().filter(|child| {
        let child_path = child["path"].as_array().unwrap();
        child_path.len() == path.len() + 1 && child_path[..path.len()] == path[..]
    }) {
        command = command.subcommand(reference_command(child));
    }
    let has_children = command.get_subcommands().next().is_some();
    command = command.arg_required_else_help(has_children && case["hasAction"] != true);
    let help = presentation::render(&command, case, catalog());
    command.override_help(help)
}
fn value_arg(name: &'static str) -> Arg {
    Arg::new(name).long(name).required(true).num_args(1)
}
pub fn command() -> Command {
    let mut root = reference_command(&catalog()[0]);
    root = root
        .subcommand(
            Command::new("build-info")
                .hide(true)
                .about("Native build and executable identity")
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("discover")
                .hide(true)
                .about("Discover project launch items")
                .arg(Arg::new("root").long("root").num_args(1))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("bridge")
                .hide(true)
                .about("Native editor helper JSON request over stdin"),
        )
        .subcommand(
            Command::new("reports")
                .hide(true)
                .about("Parse local JUnit report artifacts")
                .arg(Arg::new("path").required(true))
                .arg(Arg::new("json").long("json").action(ArgAction::SetTrue)),
        )
        .subcommand(
            Command::new("release")
                .hide(true)
                .subcommand(Command::new("verify").args([
                    value_arg("manifest"),
                    value_arg("manifest-sha256"),
                    value_arg("archive"),
                    value_arg("executable"),
                    value_arg("target"),
                ])),
        )
        .subcommand(
            Command::new("supervise")
                .hide(true)
                .arg(value_arg("install-root"))
                .arg(Arg::new("args").last(true).num_args(1..).required(true)),
        );
    root.bin_name("gridstator")
}
fn text<'a>(matches: &'a ArgMatches, name: &str) -> Option<&'a str> {
    matches
        .try_get_one::<String>(name)
        .ok()
        .flatten()
        .map(String::as_str)
}
fn values(matches: &ArgMatches, name: &str) -> Vec<String> {
    matches
        .try_get_many::<String>(name)
        .ok()
        .flatten()
        .map(|values| values.cloned().collect())
        .unwrap_or_default()
}
fn flag(matches: &ArgMatches, name: &str) -> bool {
    matches
        .try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}
fn required<'a>(matches: &'a ArgMatches, name: &str) -> Result<&'a str> {
    text(matches, name).with_context(|| format!("BAD_REQUEST: missing {name}"))
}
fn output(value: &Value) -> Result<()> {
    writeln!(
        std::io::stdout(),
        "{}",
        serde_json::to_string_pretty(value)?
    )?;
    Ok(())
}
fn printable(value: &Value, json_mode: bool) -> Result<()> {
    if json_mode {
        return output(value);
    }
    if let Some(rows) = value.as_array() {
        for row in rows {
            println!(
                "{}\t{}\t{}",
                row["id"].as_str().unwrap_or(""),
                row["name"].as_str().unwrap_or(""),
                row["state"]
                    .as_str()
                    .or_else(|| row["status"].as_str())
                    .unwrap_or("")
            );
        }
        Ok(())
    } else {
        output(value)
    }
}
fn js_whitespace(character: char) -> bool {
    (character.is_whitespace() && character != '\u{0085}') || character == '\u{feff}'
}
fn finite_js_number(input: &str) -> Option<f64> {
    let text = input.trim_matches(js_whitespace);
    let value = if text.is_empty() {
        0.0
    } else if let Some((digits, radix)) = [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ]
    .into_iter()
    .find_map(|(prefix, radix)| text.strip_prefix(prefix).map(|digits| (digits, radix)))
    {
        if digits.is_empty() {
            return None;
        }
        digits.chars().try_fold(0.0, |value, digit| {
            digit
                .to_digit(radix)
                .map(|digit| value * f64::from(radix) + f64::from(digit))
        })?
    } else {
        static DECIMAL: OnceLock<regex::Regex> = OnceLock::new();
        if !DECIMAL
            .get_or_init(|| {
                regex::Regex::new(r"^[+-]?(?:[0-9]+\.?[0-9]*|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$")
                    .unwrap()
            })
            .is_match(text)
        {
            return None;
        }
        text.parse::<f64>().ok()?
    };
    value.is_finite().then_some(value)
}
fn number(matches: &ArgMatches, name: &str) -> Result<Option<f64>> {
    text(matches, name)
        .map(|value| {
            finite_js_number(value)
                .with_context(|| format!("BAD_REQUEST: --{name} requires a finite number"))
        })
        .transpose()
}

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}
fn path(role: &str, matches: &ArgMatches) -> PathBuf {
    text(matches, "data-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| state::data_dir(role))
}
fn error_code(error: &anyhow::Error) -> i32 {
    if let Some(remote) = error
        .chain()
        .find_map(|error| error.downcast_ref::<crate::client::ControllerError>())
    {
        return match remote.code.as_deref() {
            Some("BAD_REQUEST" | "CONFIG") => 2,
            Some("UNAUTHORIZED" | "FORBIDDEN" | "TLS_PIN_MISMATCH") => 4,
            Some("NOT_FOUND") => 5,
            Some("UNAVAILABLE") => 3,
            Some("CONFLICT" | "PAYLOAD_TOO_LARGE" | "INTERNAL") => 1,
            _ => match remote.status {
                400 => 2,
                401 | 403 | 495 => 4,
                404 => 5,
                503 => 3,
                _ => 1,
            },
        };
    }
    if let Some(http) = error
        .chain()
        .find_map(|error| error.downcast_ref::<reqwest::Error>())
    {
        if let Some(status) = http.status() {
            return match status.as_u16() {
                400 => 2,
                401 | 403 | 495 => 4,
                404 => 5,
                503 => 3,
                _ => 1,
            };
        }
        if http.is_connect() || http.is_timeout() {
            return 3;
        }
    }
    let message = format!("{error:#}");
    if message.contains("401:")
        || message.contains("403:")
        || message.contains("495:")
        || message.contains("UNAUTHORIZED")
        || message.contains("FORBIDDEN")
        || message.contains("fingerprint mismatch")
        || message.contains("TLS_PIN_MISMATCH")
    {
        4
    } else if message.contains("404:") || message.contains("NOT_FOUND") {
        5
    } else if message.contains("400:")
        || message.contains("BAD_REQUEST")
        || message.contains("CONFIG")
        || message.contains("not connected")
        || message.contains("missing controller token")
        || message.contains("missing pinned cluster CA")
    {
        2
    } else if message.contains("503:") || message.contains("UNAVAILABLE") {
        3
    } else {
        1
    }
}
fn render_failure(error: &anyhow::Error) -> String {
    if let Some(remote) = error
        .chain()
        .find_map(|error| error.downcast_ref::<crate::client::ControllerError>())
    {
        let mut rendered = format!("error: {}\n", remote.message);
        if let Some(hint) = remote.hint.as_ref().filter(|hint| !hint.is_empty()) {
            rendered.push_str(&format!("hint: {hint}\n"));
        }
        rendered
    } else {
        let rendered = format!("{error:#}");
        let rendered = [
            "BAD_REQUEST:",
            "CONFIG:",
            "NOT_FOUND:",
            "FORBIDDEN:",
            "UNAUTHORIZED:",
            "CONFLICT:",
            "UNAVAILABLE:",
            "INTERNAL:",
        ]
        .into_iter()
        .find_map(|prefix| rendered.strip_prefix(prefix).map(str::trim_start))
        .unwrap_or(&rendered);
        format!("error: {rendered}\n")
    }
}
pub async fn run() -> i32 {
    let args = std::env::args_os()
        .skip(1)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let registry = command();
    let parsed_args = match parse::arguments(&args, catalog()) {
        parse::Parsed::Arguments(arguments) => arguments,
        parse::Parsed::Native => args.clone(),
        parse::Parsed::Version => {
            println!("{}", crate::VERSION);
            return 0;
        }
        parse::Parsed::Error(message) => {
            eprint!("{message}");
            return 2;
        }
        parse::Parsed::Help { path, error } => {
            let mut current = &registry;
            for name in &path {
                current = current
                    .find_subcommand(name)
                    .expect("registered help command");
            }
            let help = current.clone().render_help().to_string();
            if error {
                eprint!("{help}");
                return 2;
            }
            print!("{help}");
            return 0;
        }
    };
    let matches = match registry
        .try_get_matches_from(std::iter::once("gridstator".to_owned()).chain(parsed_args))
    {
        Ok(matches) => matches,
        Err(error) => {
            let code = if error.use_stderr() { 2 } else { 0 };
            if error.kind() == clap::error::ErrorKind::DisplayVersion {
                println!("{}", crate::VERSION);
            } else {
                let _ = error.print();
            }
            return code;
        }
    };
    match dispatch(&matches).await {
        Ok(code) => code,
        Err(error) => {
            eprint!("{}", render_failure(&error));
            error_code(&error)
        }
    }
}

struct Project {
    root: PathBuf,
    workdir: String,
    config: Value,
}
fn locate(start: &Path) -> Result<Project> {
    let absolute = fs::canonicalize(start)?;
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(&absolute)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| PathBuf::from(path.trim()));
    let mut directory = absolute.clone();
    let mut config_path = None;
    loop {
        for name in ["gridstator.yml", "gridstator.yaml"] {
            let candidate = directory.join(name);
            if candidate.exists() {
                config_path = Some(candidate);
                break;
            }
        }
        if config_path.is_some() || git.as_ref() == Some(&directory) {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent.to_owned();
    }
    let root = config_path
        .as_ref()
        .and_then(|path| path.parent())
        .map(Path::to_owned)
        .or(git)
        .unwrap_or_else(|| absolute.clone());
    let relative = absolute
        .strip_prefix(&root)
        .context("project root does not contain working directory")?;
    let workdir = if relative.as_os_str().is_empty() {
        ".".into()
    } else {
        relative.to_string_lossy().replace('\\', "/")
    };
    let raw = match config_path {
        Some(path) => {
            let raw: Value = serde_yaml_ng::from_slice(&fs::read(path)?)
                .context("CONFIG: cannot parse project YAML")?;
            if raw.is_null() { json!({}) } else { raw }
        }
        None => json!({}),
    };
    Ok(Project {
        root,
        workdir,
        config: discovery::normalize_project_config(raw)
            .context("CONFIG: invalid project configuration")?,
    })
}
fn bytes(input: &str) -> Result<f64> {
    let pattern = regex::Regex::new(r"^\s*([0-9]+(?:\.[0-9]+)?)\s*([a-zA-Z]*)\s*$")?;
    let normalized = input.replace('\u{feff}', " ");
    let captures = pattern
        .captures(&normalized)
        .filter(|_| !input.contains('\u{0085}'))
        .with_context(|| format!("Invalid size \"{input}\" (expected e.g. 512m, 8g)"))?;
    let exponent = match captures[2].to_ascii_lowercase().as_str() {
        "" | "b" => 0,
        "k" | "kb" | "ki" | "kib" => 1,
        "m" | "mb" | "mi" | "mib" => 2,
        "g" | "gb" | "gi" | "gib" => 3,
        "t" | "tb" | "ti" | "tib" => 4,
        _ => bail!("Invalid size unit \"{}\" in \"{input}\"", &captures[2]),
    };
    Ok((captures[1].parse::<f64>()? * 1024f64.powi(exponent) + 0.5).floor())
}
fn megabytes(input: &Value) -> Result<f64> {
    if let Some(number) = input.as_f64() {
        return Ok(number);
    }
    let text = input
        .as_str()
        .context("BAD_REQUEST: invalid memory value")?;
    if text
        .trim_matches(js_whitespace)
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        && !text.trim_matches(js_whitespace).is_empty()
    {
        return Ok(text.trim_matches(js_whitespace).parse()?);
    }
    Ok((bytes(text)? / 1024f64.powi(2) + 0.5).floor())
}
fn duration(input: &str) -> Result<f64> {
    let pattern = regex::Regex::new(r"(\d+(?:\.\d+)?)(ms|s|m|h|d)")?;
    let trimmed = input.trim_matches(js_whitespace);
    let mut consumed = 0;
    let mut total = 0.0;
    for captures in pattern.captures_iter(trimmed) {
        consumed += captures[0].len();
        total += captures[1].parse::<f64>()?
            * match &captures[2] {
                "ms" => 1.0,
                "s" => 1000.0,
                "m" => 60000.0,
                "h" => 3600000.0,
                _ => 86400000.0,
            };
    }
    ensure!(
        consumed > 0 && consumed == trimmed.len(),
        "Invalid duration \"{input}\" (expected e.g. 30s, 15m, 24h)"
    );
    Ok((total + 0.5).floor())
}
fn json_numeric(number: f64) -> Value {
    if number.fract() == 0.0 && number >= i64::MIN as f64 && number < i64::MAX as f64 {
        json!(number as i64)
    } else {
        json!(number)
    }
}
fn normalized(base: &str, path: &str) -> Result<String> {
    // Source posix.join concatenates before normalization, so an absolute-looking second
    // operand remains relative to this project. Parent traversal is still checked below.
    let joined = PathBuf::from(format!("{base}/{path}"));
    let mut components = Vec::new();
    for component in joined.components() {
        match component {
            Component::Normal(part) => components.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(
                    !components.is_empty(),
                    "BAD_REQUEST: path is outside the project"
                );
                components.pop();
            }
            _ => bail!("BAD_REQUEST: absolute project path"),
        }
    }
    Ok(if components.is_empty() {
        ".".into()
    } else {
        components.join("/")
    })
}
fn pairs(
    items: Vec<String>,
    allow_environment: bool,
    lookup: &(impl Fn(&str) -> Option<String> + Sync),
) -> Result<Value> {
    let mut result = Map::new();
    for item in items {
        let (name, value) = match item.split_once('=') {
            Some((name, value)) if !name.is_empty() => (name.to_owned(), value.to_owned()),
            _ if allow_environment => (
                item.clone(),
                lookup(&item).with_context(|| {
                    format!("BAD_REQUEST: environment variable {item} is not set")
                })?,
            ),
            _ => bail!("BAD_REQUEST: expected key=value"),
        };
        result.insert(name, json!(value));
    }
    Ok(Value::Object(result))
}
fn merge(into: &mut Value, other: &Value) {
    if let Some(object) = other.as_object() {
        for (key, value) in object {
            into[key] = value.clone();
        }
    }
}
fn array_strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn dedup(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert(item.clone()))
        .collect()
}
fn secrets(names: Vec<String>, lookup: &(impl Fn(&str) -> Option<String> + Sync)) -> Result<Value> {
    let mut result = json!({});
    for name in names {
        result[&name] = json!(lookup(&name).with_context(|| format!(
            "BAD_REQUEST: Secret {name} is not set in this shell's environment"
        ))?);
    }
    Ok(result)
}
fn parse_labels(input: &str) -> Result<Vec<String>> {
    static LABEL: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = LABEL.get_or_init(|| {
        regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*(=[A-Za-z0-9._:/-]+)?$").unwrap()
    });
    input
        .split(',')
        .map(|label| label.trim_matches(js_whitespace))
        .filter(|label| !label.is_empty())
        .map(|label| {
            ensure!(
                label.chars().count() <= 128 && pattern.is_match(label),
                "BAD_REQUEST: Invalid label \"{label}\""
            );
            Ok(label.to_owned())
        })
        .collect()
}
fn placement(matches: &ArgMatches, project: &Project, definition: &Value) -> Result<Value> {
    let defaults = &project.config["defaults"];
    let mut resources = defaults["resources"].clone();
    if !resources.is_object() {
        resources = json!({});
    }
    merge(&mut resources, &definition["resources"]);
    let cpus = number(matches, "cpus")?
        .or_else(|| resources["cpus"].as_f64())
        .unwrap_or(2.0);
    let memory = text(matches, "memory")
        .map(|memory| json!(memory))
        .or_else(|| resources.get("memory").cloned())
        .unwrap_or(json!(4096));
    let mut labels = array_strings(&defaults["labels"]);
    labels.extend(array_strings(&definition["labels"]));
    for value in values(matches, "label") {
        labels.extend(parse_labels(&value)?);
    }
    let mut prefer = array_strings(&defaults["prefer"]);
    prefer.extend(array_strings(&definition["prefer"]));
    prefer.extend(values(matches, "prefer"));
    let mut target = json!({"labels":dedup(labels),"prefer":dedup(prefer)});
    if let Some(node) = text(matches, "node")
        .or_else(|| definition["node"].as_str())
        .filter(|node| !node.is_empty())
    {
        target["node"] = json!(node);
    }
    let priority = number(matches, "priority")?
        .or_else(|| definition["priority"].as_f64())
        .unwrap_or(0.0);
    let mut result = json!({"resources":{"cpus":json_numeric(cpus),"memoryMb":json_numeric(megabytes(&memory)?)},"target":target,"priority":json_numeric(priority),"project":project.config["project"].as_str().unwrap_or_else(||project.root.file_name().and_then(|name|name.to_str()).unwrap_or("project"))});
    if let Some(timeout) = text(matches, "timeout")
        .or_else(|| definition["timeout"].as_str())
        .or_else(|| defaults["timeout"].as_str())
        && !timeout.is_empty()
    {
        result["timeoutSeconds"] = json_numeric((duration(timeout)? / 1000.0).ceil());
    }
    if let Some(name) = text(matches, "name").filter(|name| !name.is_empty()) {
        result["name"] = json!(name);
    }
    Ok(result)
}
async fn capture_project(project: &Project, quiet: bool) -> Result<Arc<workspace::Capture>> {
    let snapshot = &project.config["snapshot"];
    let maximum = if let Some(number) = snapshot["maxFileSize"].as_u64() {
        number
            .checked_mul(1024 * 1024)
            .context("CONFIG: snapshot maximum overflow")?
    } else {
        bytes(snapshot["maxFileSize"].as_str().unwrap_or("256m"))? as u64
    };
    let options = workspace::CaptureOptions {
        include: array_strings(&snapshot["include"]),
        exclude: array_strings(&snapshot["exclude"]),
        allow_secrets: array_strings(&snapshot["allowSecrets"]),
        max_file_size: maximum,
        cache_dir: Some(
            dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("gridstator-v2/snapshot"),
        ),
        ..Default::default()
    };
    let root = project.root.clone();
    let captured =
        tokio::task::spawn_blocking(move || workspace::capture(&root, options)).await??;
    if !quiet {
        eprintln!(
            "Snapshot of {}: {} files, {} bytes",
            project.root.display(),
            captured.stats.files,
            captured.stats.total_bytes
        );
        for warning in &captured.warnings {
            eprintln!("  ! {}: {}", warning.path, warning.message);
        }
    }
    Ok(Arc::new(captured))
}
async fn upload(client: &Client, captured: Arc<workspace::Capture>, quiet: bool) -> Result<()> {
    let summary = crate::transfer::upload_capture_with_summary(client, captured).await?;
    if !quiet {
        if summary.uploaded_blobs == 0 {
            eprintln!("Snapshot already on the controller (nothing to upload)");
        } else {
            eprintln!(
                "Uploading {} new blob(s), {}...",
                summary.uploaded_blobs,
                output_bytes(summary.uploaded_bytes as f64)
            );
        }
    }
    Ok(())
}

fn exit_for_job(job: &Value) -> i32 {
    match job["state"].as_str() {
        Some("succeeded") => 0,
        Some("cancelled") => 130,
        Some("lost") => 75,
        Some("failed") => job["exitCode"]
            .as_i64()
            .filter(|code| *code > 0 && *code < 256)
            .unwrap_or(1) as i32,
        _ => 1,
    }
}
fn write_logs(entries: &[Value], json_mode: bool) -> Result<u64> {
    let mut sequence = 0;
    for entry in entries {
        sequence = sequence.max(entry["s"].as_u64().unwrap_or(0));
        if json_mode {
            writeln!(std::io::stdout(), "{}", serde_json::to_string(entry)?)?;
        } else if let Some(data) = entry["d"].as_str() {
            if entry["o"] == "stdout" {
                std::io::stdout().write_all(data.as_bytes())?;
            } else if entry["o"] == "stderr" {
                std::io::stderr().write_all(data.as_bytes())?;
            } else {
                for line in data.strip_suffix('\n').unwrap_or(data).split('\n') {
                    writeln!(std::io::stderr(), "[gridstator] {line}")?;
                }
            }
        }
    }
    Ok(sequence)
}
async fn follow(
    client: &Client,
    id: &str,
    cancel_on_interrupt: bool,
    json_logs: bool,
    wait: bool,
    summarize_outputs: bool,
) -> Result<i32> {
    let mut after = 0;
    let mut cancelled = false;
    loop {
        let page = client
            .get(&format!(
                "/v1/jobs/{}/logs?after={after}&limit=10000",
                encode(id)
            ))
            .await?;
        let entries = page["entries"].as_array().context("Invalid log response")?;
        after = after.max(write_logs(entries, json_logs)?);
        if entries.len() == 10000 {
            continue;
        }
        if !wait {
            return Ok(0);
        }
        if page["complete"] == true {
            let final_job = client.get(&format!("/v1/jobs/{}", encode(id))).await?;
            if after >= final_job["logSeq"].as_u64().unwrap_or(0) {
                for line in terminal_summary_lines(&final_job) {
                    eprintln!("{line}");
                }
                if summarize_outputs {
                    for line in workload_output_lines(&final_job) {
                        eprintln!("{line}");
                    }
                }
                return Ok(exit_for_job(&final_job));
            }
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_millis(300))=>{},_=tokio::signal::ctrl_c()=>{
            if !cancel_on_interrupt||cancelled{eprintln!("Detached; use gridstator logs -f {id} to follow.");return Ok(130);}
            client.post(&format!("/v1/jobs/{}/cancel",encode(id)),json!({})).await?;cancelled=true;eprintln!("Cancellation requested (Ctrl-C again to detach).");
        }}
    }
}
fn fixed_decimal(value: f64, precision: usize) -> String {
    let bits = value.abs().to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023 - 52;
    let significand = (bits & ((1_u64 << 52) - 1))
        | if bits >> 52 & 0x7ff == 0 {
            0
        } else {
            1_u64 << 52
        };
    let numerator = u128::from(significand) * 10_u128.pow(precision as u32);
    let rounded = if exponent >= 0 {
        numerator.checked_shl(exponent as u32).unwrap_or(0)
    } else {
        let shift = (-exponent) as u32;
        if shift >= 128 {
            0
        } else {
            let divisor = 1_u128 << shift;
            let quotient = numerator / divisor;
            let remainder = numerator % divisor;
            quotient + u128::from(remainder >= divisor - remainder)
        }
    };
    let sign = if value < 0.0 { "-" } else { "" };
    if precision == 0 {
        format!("{sign}{rounded}")
    } else {
        let divisor = 10_u128.pow(precision as u32);
        format!(
            "{sign}{}.{:0precision$}",
            rounded / divisor,
            rounded % divisor
        )
    }
}
fn output_duration(ms: f64) -> String {
    if !ms.is_finite() || ms < 0.0 {
        return "-".into();
    }
    if ms < 1000.0 {
        return format!("{}ms", fixed_decimal(ms, 0));
    }
    let seconds = (ms / 1000.0).floor() as u64;
    if seconds < 60 {
        return format!("{}s", fixed_decimal(ms / 1000.0, usize::from(seconds < 10)));
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        format!("{minutes}m{:02}s", seconds % 60)
    } else {
        format!("{}h{:02}m", minutes / 60, minutes % 60)
    }
}
fn terminal_summary_lines(job: &Value) -> Vec<String> {
    let id = job["id"].as_str().unwrap_or("");
    let state = job["state"].as_str().unwrap_or("finished");
    let mut summary = format!("Job {id} {state}");
    if let Some(node) = job["nodeName"].as_str().filter(|node| !node.is_empty()) {
        summary.push_str(&format!(" on {node}"));
    }
    if let Some(start) = job["startedAt"].as_f64().filter(|start| *start != 0.0) {
        let end = job["finishedAt"].as_f64().unwrap_or(state::now() as f64);
        summary.push_str(&format!(" in {}", output_duration(end - start)));
    }
    if state == "failed"
        && let Some(exit) = job["exitCode"].as_i64()
    {
        summary.push_str(&format!(" (exit code {exit})"));
    }
    let mut lines = vec![summary];
    if state != "succeeded"
        && let Some(error) = job["error"].as_str().filter(|error| !error.is_empty())
    {
        lines.push(format!("  {error}"));
    }
    if state == "lost" {
        lines.push(format!("  Retry with: gridstator retry {id}"));
    }
    lines
}
fn output_bytes(bytes: f64) -> String {
    if !bytes.is_finite() {
        return "-".into();
    }
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value.abs() >= 1024.0 && unit < units.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let precision = usize::from(value < 10.0 && unit != 0);
    format!("{} {}", fixed_decimal(value, precision), units[unit])
}
fn workload_output_lines(job: &Value) -> Vec<String> {
    let kind = job["kind"]
        .as_str()
        .or_else(|| job["spec"]["kind"].as_str());
    if !matches!(kind, Some("docker-build" | "actions")) {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let image = &job["outputs"]["image"];
    if job["state"] == "succeeded"
        && let Some(id) = image["id"].as_str().filter(|id| !id.is_empty())
    {
        let tags = array_strings(&image["tags"]);
        let tags = if tags.is_empty() {
            String::new()
        } else {
            format!(" ({})", tags.join(", "))
        };
        let size = image["sizeBytes"]
            .as_f64()
            .map(|size| format!(", {}", output_bytes(size)))
            .unwrap_or_default();
        let node = job["nodeName"].as_str().unwrap_or("null");
        lines.push(format!("Image {id}{tags}{size} on {node}"));
    }
    for artifact in job["artifacts"].as_array().into_iter().flatten() {
        if let (Some(name), Some(size)) = (artifact["name"].as_str(), artifact["size"].as_f64()) {
            lines.push(format!(
                "Artifact {name} ({}): gridstator artifacts {} --download {name}",
                output_bytes(size),
                job["id"].as_str().unwrap_or("")
            ));
        }
    }
    lines
}
async fn finish_submission(
    client: &Client,
    job: Value,
    matches: &ArgMatches,
    group: bool,
) -> Result<i32> {
    if flag(matches, "json") {
        output(&job)?;
        return Ok(0);
    }
    let id = job["id"].as_str().context("Missing submitted job id")?;
    if flag(matches, "detach") {
        println!("{id}");
        return Ok(0);
    }
    eprintln!("{} {id} submitted", if group { "Group" } else { "Job" });
    if group {
        follow_group(client, id).await
    } else {
        follow(client, id, true, false, true, true).await
    }
}
async fn follow_group(client: &Client, id: &str) -> Result<i32> {
    let mut sequences = std::collections::HashMap::<String, u64>::new();
    let mut cancelled = false;
    loop {
        let group = client.get(&format!("/v1/groups/{}", encode(id))).await?;
        let jobs = group["jobs"].as_array().context("Invalid group response")?;
        for job in jobs {
            let id = job["id"].as_str().context("Invalid shard job id")?;
            let mut after = sequences.get(id).copied().unwrap_or(0);
            loop {
                let page = client
                    .get(&format!(
                        "/v1/jobs/{}/logs?after={after}&limit=10000",
                        encode(id)
                    ))
                    .await?;
                let entries = page["entries"].as_array().context("Invalid shard logs")?;
                for entry in entries {
                    let data = entry["d"].as_str().unwrap_or("");
                    let prefix = format!(
                        "[{}/{}]",
                        job["shardIndex"].as_u64().unwrap_or(0),
                        job["shardTotal"].as_u64().unwrap_or(0)
                    );
                    for line in data.strip_suffix('\n').unwrap_or(data).split('\n') {
                        if entry["o"] == "stdout" {
                            writeln!(std::io::stdout(), "{prefix} {line}")?;
                        } else if entry["o"] == "stderr" {
                            writeln!(std::io::stderr(), "{prefix} {line}")?;
                        } else {
                            writeln!(std::io::stderr(), "{prefix} [gridstator] {line}")?;
                        }
                    }
                    after = after.max(entry["s"].as_u64().unwrap_or(0));
                }
                sequences.insert(id.into(), after);
                if entries.len() < 10000 {
                    break;
                }
            }
        }
        if protocol::terminal(group["state"].as_str().unwrap_or(""))
            && jobs.iter().all(|job| {
                sequences
                    .get(job["id"].as_str().unwrap_or(""))
                    .copied()
                    .unwrap_or(0)
                    >= job["logSeq"].as_u64().unwrap_or(0)
            })
        {
            let tests = group_reports(client, &group).await?;
            if !tests.is_null() {
                eprintln!(
                    "Tests: {} passed, {} failed, {} skipped",
                    tests["passed"], tests["failed"], tests["skipped"]
                );
            }
            return Ok(jobs
                .iter()
                .map(exit_for_job)
                .find(|code| *code != 0)
                .unwrap_or(0));
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_millis(300))=>{},_=tokio::signal::ctrl_c()=>{if cancelled{return Ok(130);}client.post(&format!("/v1/groups/{}/cancel",encode(id)),json!({})).await?;cancelled=true;}}
    }
}
async fn prepare_workload(
    kind: &str,
    matches: &ArgMatches,
    project: &Project,
    lookup: &(impl Fn(&str) -> Option<String> + Sync),
) -> Result<(Value, u64)> {
    let definition = match kind {
        "command" => match text(matches, "task").filter(|name| !name.is_empty()) {
            Some(name) => project.config["tasks"]
                .get(name)
                .cloned()
                .with_context(|| format!("BAD_REQUEST: No task named {name}"))?,
            None => json!({}),
        },
        "docker-build" => match text(matches, "build").filter(|name| !name.is_empty()) {
            Some(name) => project.config["builds"]
                .get(name)
                .cloned()
                .with_context(|| format!("BAD_REQUEST: No build named {name}"))?,
            None => json!({}),
        },
        _ => json!({}),
    };
    let mut spec = placement(matches, project, &definition)?;
    spec["kind"] = json!(kind);
    match kind {
        "command" => {
            let arguments = values(matches, "command");
            let source = if !arguments.is_empty() {
                json!(arguments)
            } else {
                definition["command"].clone()
            };
            let argv = if let Some(shell) = source.as_str() {
                vec!["sh".into(), "-c".into(), shell.into()]
            } else {
                let args = array_strings(&source);
                ensure!(
                    !args.is_empty(),
                    "BAD_REQUEST: Nothing to run; use run --task NAME or run -- COMMAND"
                );
                if flag(matches, "shell") || args.len() == 1 {
                    vec!["sh".into(), "-c".into(), args.join(" ")]
                } else {
                    args
                }
            };
            spec["command"] = json!(argv);
            spec["image"] = json!(
                text(matches, "image")
                    .or_else(|| definition["image"].as_str())
                    .or_else(|| project.config["defaults"]["image"].as_str())
                    .unwrap_or("node:24-bookworm")
            );
            let workdir = definition["workdir"]
                .as_str()
                .filter(|workdir| !workdir.is_empty())
                .map(|workdir| {
                    workdir
                        .strip_prefix('.')
                        .map(|tail| tail.strip_prefix('/').unwrap_or(tail))
                        .unwrap_or(workdir)
                })
                .unwrap_or(&project.workdir);
            spec["workdir"] = json!(if workdir.is_empty() { "." } else { workdir });
            let mut environment = definition["env"].clone();
            if !environment.is_object() {
                environment = json!({});
            }
            merge(
                &mut environment,
                &pairs(values(matches, "env"), true, lookup)?,
            );
            spec["env"] = environment;
            let mut artifacts = array_strings(&definition["artifacts"])
                .into_iter()
                .map(|path| normalized(".", &path))
                .collect::<Result<Vec<_>>>()?;
            if let Some(junit) = definition["junit"].as_str().filter(|path| !path.is_empty()) {
                artifacts.push(normalized(".", junit)?);
            }
            for artifact in values(matches, "artifact") {
                artifacts.push(normalized(&project.workdir, &artifact)?);
            }
            spec["artifacts"] = json!(dedup(artifacts));
            spec["retry"] = json!({"max":json_numeric(number(matches,"retries")?.or_else(||definition["retry"]["max"].as_f64()).unwrap_or(0.0)),"on":definition["retry"].get("on").cloned().unwrap_or(json!(["lost"]))});
            if text(matches, "name")
                .filter(|name| !name.is_empty())
                .is_none()
                && let Some(name) = text(matches, "task").filter(|name| !name.is_empty())
            {
                spec["name"] = json!(format!("task:{name}"));
            }
        }
        "docker-build" => {
            spec["context"] = json!(match text(matches, "context") {
                Some(path) => normalized(&project.workdir, path)?,
                None => match definition["context"].as_str() {
                    Some(path) => normalized(".", path)?,
                    None => project.workdir.clone(),
                },
            });
            if let Some(file) = text(matches, "file") {
                spec["dockerfile"] = json!(normalized(&project.workdir, file)?);
            } else if let Some(file) = definition["file"].as_str() {
                spec["dockerfile"] = json!(normalized(".", file)?);
            }
            if let Some(stage) = text(matches, "target").or_else(|| definition["stage"].as_str()) {
                spec["stage"] = json!(stage);
            }
            let mut build_args = definition["buildArgs"].clone();
            if !build_args.is_object() {
                build_args = json!({});
            }
            merge(
                &mut build_args,
                &pairs(values(matches, "build-arg"), false, lookup)?,
            );
            spec["buildArgs"] = build_args;
            for (flag_name, field) in [("tag", "tags"), ("platform", "platforms")] {
                let items = values(matches, flag_name);
                spec[field] = if items.is_empty() {
                    definition.get(field).cloned().unwrap_or(json!([]))
                } else {
                    json!(items)
                };
            }
            spec["output"] = json!(
                text(matches, "output")
                    .or_else(|| definition["output"].as_str())
                    .unwrap_or("load")
            );
            spec["noCache"] = json!(flag(matches, "no-cache"));
            spec["pull"] = json!(flag(matches, "pull"));
            spec["cacheFrom"] = json!(values(matches, "cache-from"));
            spec["cacheTo"] = json!(values(matches, "cache-to"));
            if text(matches, "name")
                .filter(|name| !name.is_empty())
                .is_none()
                && let Some(name) = text(matches, "build").filter(|name| !name.is_empty())
            {
                spec["name"] = json!(format!("build:{name}"));
            }
        }
        "actions" => {
            let workflows = discovery::discover_workflows(&project.root)?;
            let query = required(matches, "workflow")?;
            let workflow = discovery::find_workflow(&workflows, query)
                .with_context(|| format!("NOT_FOUND: No workflow {query}"))?;
            ensure!(
                workflow.get("parseError").is_none(),
                "BAD_REQUEST: Cannot parse workflow: {}",
                workflow["parseError"]
            );
            let jobs = workflow["jobs"].as_array().context("workflow jobs")?;
            let selected = jobs
                .iter()
                .filter(|job| text(matches, "job").is_none_or(|id| job["id"] == id))
                .collect::<Vec<_>>();
            ensure!(
                text(matches, "job").is_none() || !selected.is_empty(),
                "NOT_FOUND: Workflow has no selected job"
            );
            ensure!(
                selected.is_empty() || selected.iter().any(|job| job["runnable"] == true),
                "BAD_REQUEST: act runs Linux containers only; selected workflow jobs cannot run"
            );
            spec["workflow"] = workflow["file"].clone();
            spec["event"] = json!(
                text(matches, "event")
                    .or_else(|| workflow["defaultEvent"].as_str())
                    .unwrap_or("push")
            );
            if let Some(job) = text(matches, "job") {
                spec["job"] = json!(job);
            }
            spec["inputs"] = pairs(values(matches, "input"), false, lookup)?;
            if spec["event"] == "workflow_dispatch" {
                let missing = workflow["dispatchInputs"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|input| {
                        input["required"] == true
                            && input.get("default").is_none()
                            && !spec["inputs"]
                                .as_object()
                                .unwrap()
                                .contains_key(input["name"].as_str().unwrap())
                    })
                    .map(|input| input["name"].as_str().unwrap())
                    .collect::<Vec<_>>();
                ensure!(
                    missing.is_empty(),
                    "BAD_REQUEST: Missing required input(s): {}\nhint: Pass them with --input name=value.",
                    missing.join(", ")
                );
            }
            if !flag(matches, "json") {
                for warning in workflow["warnings"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    eprintln!("! {warning}");
                }
                for job in &selected {
                    for warning in job["warnings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                    {
                        eprintln!("! {}: {warning}", job["id"].as_str().unwrap_or(""));
                    }
                }
            }

            spec["env"] = pairs(values(matches, "env"), true, lookup)?;
            let actions = &project.config["actions"];
            let mut vars = actions["vars"].clone();
            if !vars.is_object() {
                vars = json!({});
            }
            merge(&mut vars, &pairs(values(matches, "var"), false, lookup)?);
            spec["vars"] = vars;
            let mut platforms = actions["platforms"].clone();
            if !platforms.is_object() {
                platforms = json!({});
            }
            merge(
                &mut platforms,
                &pairs(values(matches, "platform"), false, lookup)?,
            );
            spec["platforms"] = platforms;
            spec["matrix"] = json!(values(matches, "matrix"));
            spec["dockerSocket"] =
                json!(flag(matches, "docker-socket") || actions["dockerSocket"] == true);
            if let Some(payload) = text(matches, "event-payload") {
                spec["eventPayload"] = serde_json::from_slice(&fs::read(payload)?)
                    .context("BAD_REQUEST: invalid event payload JSON")?;
            }
            let mut git = json!({});
            for (key, args) in [
                ("branch", vec!["rev-parse", "--abbrev-ref", "HEAD"]),
                ("headSha", vec!["rev-parse", "HEAD"]),
                ("remoteUrl", vec!["remote", "get-url", "origin"]),
            ] {
                if let Ok(result) = tokio::process::Command::new("git")
                    .arg("-C")
                    .arg(&project.root)
                    .args(args)
                    .output()
                    .await
                    && result.status.success()
                {
                    let value = String::from_utf8_lossy(&result.stdout).trim().to_owned();
                    if !value.is_empty() && (key != "branch" || value != "HEAD") {
                        git[key] = json!(value);
                    }
                }
            }
            spec["git"] = git;
        }
        _ => bail!("BAD_REQUEST: unsupported workload kind"),
    }
    let mut secret_names = array_strings(&definition["secretEnv"]);
    if kind == "actions" {
        secret_names.extend(array_strings(&project.config["actions"]["secretEnv"]));
    }
    secret_names.extend(values(matches, "secret-env"));
    spec["secretEnv"] = secrets(secret_names, lookup)?;
    // Validate before capturing/uploading, so bad input has no remote side effects.
    let _ =
        protocol::normalize_spec(spec.clone()).context("BAD_REQUEST: invalid job specification")?;
    let shards = number(matches, "shards")?
        .or_else(|| definition["shards"].as_f64())
        .unwrap_or(1.0);
    ensure!(
        shards.fract() == 0.0 && (1.0..=64.0).contains(&shards),
        "BAD_REQUEST: shards must be 1..64"
    );
    Ok((spec, shards as u64))
}
async fn submit_workload(kind: &str, matches: &ArgMatches) -> Result<i32> {
    let cwd = std::env::current_dir()?;
    let project = tokio::task::spawn_blocking(move || locate(&cwd)).await??;
    let (mut spec, shards) =
        prepare_workload(kind, matches, &project, &|name| std::env::var(name).ok()).await?;
    let client = Client::load(None)?;
    if kind != "command" || !flag(matches, "no-snapshot") {
        let captured = capture_project(&project, flag(matches, "json")).await?;
        upload(&client, captured.clone(), flag(matches, "json")).await?;
        spec["snapshot"] = json!(captured.id);
    }
    let group = kind == "command" && shards > 1;
    let job = if group {
        client
            .post("/v1/groups", json!({"spec":spec,"shards":shards}))
            .await?
    } else {
        client.post("/v1/jobs", json!({"spec":spec})).await?
    };
    finish_submission(&client, job, matches, group).await
}
fn node_settings(matches: &ArgMatches) -> Result<Value> {
    let mut settings = json!({});
    if let Some(max) = number(matches, "max-concurrency")? {
        ensure!(
            max >= 0.0 && max.fract() == 0.0,
            "BAD_REQUEST: --max-concurrency must be a whole number"
        );
        settings["maxConcurrency"] = json_numeric(max);
    }
    if let Some(cpus) = number(matches, "reserve-cpus")? {
        ensure!(cpus >= 0.0, "BAD_REQUEST: --reserve-cpus must be a number");
        settings["reserved"]["cpus"] = json_numeric(cpus);
    }
    if let Some(memory) = text(matches, "reserve-memory") {
        settings["reserved"]["memoryMb"] = json_numeric(megabytes(&json!(memory))?);
    }
    if let Some(labels) = text(matches, "labels") {
        settings["labels"] = json!(parse_labels(labels)?);
    }
    Ok(settings)
}
async fn dispatch(matches: &ArgMatches) -> Result<i32> {
    let Some((name, command)) = matches.subcommand() else {
        command().print_help()?;
        println!();
        return Ok(0);
    };
    match name {
        "build-info" => {
            let (size, sha) = crate::install::running_executable_digest()?;
            output(
                &json!({"version":crate::VERSION,"buildId":crate::BUILD_ID,"sourceRevision":crate::SOURCE_REVISION,"capabilities":crate::bridge::CAPABILITIES,"target":crate::release::host_target()?,"size":size,"sha256":sha}),
            )?;
        }
        "bridge" => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::io::stdin()
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= 16 * 1024 * 1024,
                "BAD_REQUEST: bridge request exceeds 16 MiB"
            );
            output(
                &crate::bridge::execute(
                    serde_json::from_slice(&bytes).context("BAD_REQUEST: invalid bridge JSON")?,
                )
                .await?,
            )?;
        }
        "discover" => {
            let root = text(command, "root")
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?);
            output(&tokio::task::spawn_blocking(move || discovery::discover(&root)).await??)?;
        }
        "reports" => {
            let report = PathBuf::from(required(command, "path")?);
            let name = report
                .file_name()
                .and_then(|name| name.to_str())
                .context("report filename")?
                .to_owned();
            let cases = tokio::task::spawn_blocking(move || {
                crate::reports::collect_junit(&name, &fs::read(report)?)
            })
            .await??;
            let summary = crate::reports::summarize(&[json!({"cases":cases})])?;
            output(&json!({"cases":cases,"summary":summary}))?;
        }
        "release" => {
            let args = command
                .subcommand_matches("verify")
                .context("BAD_REQUEST: use release verify")?;
            let manifest = crate::release::ReleaseManifest::read_verified(
                required(args, "manifest")?,
                required(args, "manifest-sha256")?,
            )?;
            let target = required(args, "target")?;
            ensure!(
                target == crate::release::host_target()?,
                "release target differs from executing binary"
            );
            ensure!(
                manifest.build_id == crate::BUILD_ID && manifest.version == crate::VERSION,
                "release metadata differs from executing binary"
            );
            let artifact = manifest.target(target)?;
            artifact.archive.verify(required(args, "archive")?)?;
            artifact.executable.verify(required(args, "executable")?)?;
            crate::release::verify_executable_target(required(args, "executable")?, target)?;
            output(
                &json!({"verified":true,"buildId":manifest.build_id,"version":manifest.version,"target":target}),
            )?;
        }
        "supervise" => {
            let store = crate::install::InstallStore::open(required(command, "install-root")?)?;
            let options = crate::install::SupervisorOptions {
                args: values(command, "args")
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                ..Default::default()
            };
            return Ok(crate::install::supervise(&store, options).await?.code);
        }
        "controller" => return controller_command(command).await,
        "agent" => return agent_command(command).await,
        "login" => {
            let client = Client::login(
                required(command, "url")?,
                required(command, "fingerprint")?,
                required(command, "token")?,
                &state::config_path(),
            )
            .await?;
            eprintln!(
                "Logged in to {}. Saved to {}.",
                client.config.url,
                state::config_path().display()
            );
        }
        "run" => return submit_workload("command", command).await,
        "build" => return submit_workload("docker-build", command).await,
        "actions" => match command.subcommand() {
            Some(("list", args)) => {
                let cwd = std::env::current_dir()?;
                let project = tokio::task::spawn_blocking(move || locate(&cwd)).await??;
                printable(
                    &json!(discovery::discover_workflows(&project.root)?),
                    flag(args, "json"),
                )?;
            }
            Some(("run", args)) => return submit_workload("actions", args).await,
            _ => bail!("BAD_REQUEST: use actions list or actions run"),
        },
        "snapshot" => {
            let cwd = std::env::current_dir()?;
            let project = tokio::task::spawn_blocking(move || locate(&cwd)).await??;
            let captured = capture_project(&project, flag(command, "json")).await?;
            if !flag(command, "dry-run") {
                upload(
                    &Client::load(None)?,
                    captured.clone(),
                    flag(command, "json"),
                )
                .await?;
            }
            if flag(command, "json") {
                output(
                    &json!({"id":captured.id,"root":project.root,"workdir":project.workdir,"stats":captured.stats,"warnings":captured.warnings,"entries":captured.manifest.entries,"uploaded":!flag(command,"dry-run")}),
                )?;
            } else {
                println!("{}", captured.id);
            }
        }
        "init" => {
            let file = std::env::current_dir()?.join("gridstator.yml");
            ensure!(
                flag(command, "force") || !file.exists(),
                "CONFIG: gridstator.yml already exists; use --force"
            );
            fs::write(
                file,
                "version: 1\ndefaults:\n  image: node:24-bookworm\n  resources: {cpus: 2, memory: 4g}\ntasks:\n  test:\n    command: corepack enable && pnpm install --frozen-lockfile && pnpm test\n",
            )?;
            eprintln!("Created gridstator.yml");
        }
        "doctor" => return doctor_command(command).await,
        "status" => {
            let client = Client::load(None)?;
            let (cluster, nodes, jobs) = tokio::try_join!(
                client.get("/v1/cluster"),
                client.get("/v1/nodes"),
                client.get("/v1/jobs?state=queued,assigned,starting,running&limit=100")
            )?;
            printable(
                &json!({"cluster":cluster,"nodes":nodes["nodes"],"activeJobs":jobs["jobs"]}),
                flag(command, "json"),
            )?;
        }
        "nodes" => {
            let client = Client::load(None)?;
            match command.subcommand() {
                None => {
                    let nodes = client.get("/v1/nodes").await?;
                    printable(&nodes["nodes"], flag(command, "json"))?;
                }
                Some((action, args)) => {
                    let node = required(args, "node")?;
                    let route = format!("/v1/nodes/{}", encode(node));
                    let response = match action {
                        "show" => client.get(&route).await?,
                        "remove" => client.request(reqwest::Method::DELETE, &format!("{route}?force={}", flag(args, "force")), None).await?,
                        "set" => {
                            let mut patch = node_settings(args)?;
                            if let Some(name) = text(args, "name") { patch["name"] = json!(name); }
                            ensure!(!patch.as_object().unwrap().is_empty(), "BAD_REQUEST: Nothing to change");
                            let updated = client.request(reqwest::Method::PATCH, &route, Some(patch)).await?;
                            if let Some(name) = text(args, "name") {
                                ensure!(updated["name"] == name, "UNAVAILABLE: This controller did not apply the worker name; update the controller to a release with worker renaming support");
                            }
                            updated
                        },
                        "enable" | "disable" | "drain" => client.request(reqwest::Method::PATCH, &route, Some(json!({"schedulingMode":match action { "enable"=>"enabled", "disable"=>"disabled", _=>"draining" }}))).await?,
                        _ => bail!("BAD_REQUEST: unknown node action"),
                    };
                    printable(&response, flag(args, "json"))?;
                }
            }
        }
        "jobs" => {
            let client = Client::load(None)?;
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            for key in ["state", "node", "limit"] {
                if let Some(value) = text(command, key) {
                    query.append_pair(key, value);
                }
            }
            if text(command, "limit").is_none() {
                query.append_pair("limit", "30");
            }
            let response = client.get(&format!("/v1/jobs?{}", query.finish())).await?;
            printable(&response["jobs"], flag(command, "json"))?;
        }
        "job" => {
            let client = Client::load(None)?;
            let id = encode(required(command, "id")?);
            let job_route = format!("/v1/jobs/{id}");
            let event_route = format!("/v1/jobs/{id}/events");
            let (job, events) = tokio::try_join!(client.get(&job_route), client.get(&event_route))?;
            printable(
                &json!({"job":job,"events":events["events"]}),
                flag(command, "json"),
            )?;
        }
        "logs" => {
            return follow(
                &Client::load(None)?,
                required(command, "id")?,
                false,
                flag(command, "json"),
                flag(command, "follow"),
                false,
            )
            .await;
        }
        "cancel" => {
            let id = required(command, "id")?;
            Client::load(None)?
                .post(&format!("/v1/jobs/{}/cancel", encode(id)), json!({}))
                .await?;
            eprintln!("Cancellation of {id} requested.");
        }
        "retry" => {
            let client = Client::load(None)?;
            let job = client
                .post(
                    &format!("/v1/jobs/{}/retry", encode(required(command, "id")?)),
                    json!({}),
                )
                .await?;
            return finish_submission(&client, job, command, false).await;
        }
        "token" => {
            let client = Client::load(None)?;
            let (action, args) = command
                .subcommand()
                .context("BAD_REQUEST: use token create/list/revoke")?;
            match action {
                "list" => {
                    let response = client.get("/v1/tokens").await?;
                    printable(&response["tokens"], flag(args, "json"))?;
                }
                "revoke" => {
                    client
                        .request(
                            reqwest::Method::DELETE,
                            &format!("/v1/tokens/{}", encode(required(args, "id")?)),
                            None,
                        )
                        .await?;
                    eprintln!("Token revoked.");
                }
                "create" => {
                    let join = flag(args, "join");
                    let default_name = if join {
                        format!("join-{}", chrono::Utc::now().format("%Y-%m-%d"))
                    } else {
                        "client".into()
                    };
                    let mut request = json!({"kind":if join{"join"}else{"client"},"name":text(args,"name").unwrap_or(&default_name),"role":if text(args,"role")==Some("admin"){"admin"}else{"operator"}});
                    if join {
                        request["maxUses"] = json_numeric(number(args, "uses")?.unwrap_or(1.0));
                    }
                    if let Some(ttl) = text(args, "ttl") {
                        request["ttlSeconds"] = json_numeric((duration(ttl)? / 1000.0).ceil());
                    }
                    let response = client.post("/v1/tokens", request).await?;
                    if flag(args, "json") {
                        output(&response)?;
                    } else {
                        println!("{}", response["token"].as_str().context("token response")?);
                    }
                }
                _ => bail!("BAD_REQUEST: unknown token command"),
            }
        }
        "groups" => {
            let response = Client::load(None)?
                .get(&format!(
                    "/v1/groups?limit={}",
                    text(command, "limit").unwrap_or("20")
                ))
                .await?;
            printable(&response["groups"], flag(command, "json"))?;
        }
        "group" => {
            let client = Client::load(None)?;
            let (action, args) = command
                .subcommand()
                .context("BAD_REQUEST: use group show/cancel/retry")?;
            let id = required(args, "id")?;
            let route = format!("/v1/groups/{}", encode(id));
            match action {
                "show" => {
                    let group = client.get(&route).await?;
                    let tests = group_reports(&client, &group).await?;
                    printable(&json!({"group":group,"tests":tests}), flag(args, "json"))?;
                }
                "cancel" => {
                    client.post(&format!("{route}/cancel"), json!({})).await?;
                    eprintln!("Group cancellation requested.");
                }
                "retry" => {
                    client.post(&format!("{route}/retry"), json!({})).await?;
                    if flag(args, "detach") {
                        println!("{id}");
                    } else {
                        return follow_group(&client, id).await;
                    }
                }
                _ => bail!("BAD_REQUEST: unknown group command"),
            }
        }
        "artifacts" => return artifacts_command(command).await,
        "agents" => return agents_command(command).await,
        _ => bail!("BAD_REQUEST: native command {name} is not yet implemented"),
    }
    Ok(0)
}
async fn controller_command(matches: &ArgMatches) -> Result<i32> {
    let (action, args) = matches
        .subcommand()
        .context("BAD_REQUEST: choose a controller command")?;
    let directory = path("controller", args);
    match action {
        "init" => {
            let port = text(args, "port")
                .unwrap_or("7300")
                .parse::<u16>()
                .context("BAD_REQUEST: invalid port")?;
            let no_login = flag(args, "no-login");
            let save_login = !no_login && (flag(args, "force") || !state::config_path().exists());
            let temporary = if !save_login {
                Some(tempfile::tempdir()?)
            } else {
                None
            };
            let config = temporary
                .as_ref()
                .map(|dir| dir.path().join("client.json"))
                .unwrap_or_else(state::config_path);
            let initialized = crate::controller::initialize(
                &directory,
                text(args, "host").unwrap_or("0.0.0.0"),
                port,
                &values(args, "san"),
                flag(args, "force"),
                &config,
            )
            .await?;
            eprintln!("Cluster initialized in {}", directory.display());
            eprintln!("Controller: {}", initialized["url"].as_str().unwrap_or(""));
            eprintln!(
                "CA fingerprint: {}",
                initialized["caFingerprint"].as_str().unwrap_or("")
            );
            eprintln!(
                "Admin token: {}",
                initialized["token"].as_str().unwrap_or("")
            );
            if save_login {
                eprintln!("Saved local credentials to {}", config.display());
            }
        }
        "start" => {
            let host = text(args, "host").map(str::to_owned);
            let port = text(args, "port")
                .map(str::parse::<u16>)
                .transpose()
                .context("BAD_REQUEST: invalid port")?;
            if flag(args, "with-agent") {
                let settings = node_settings(args)?;
                crate::controller::start_embedded(
                    directory,
                    host,
                    port,
                    text(args, "agent-name").map(str::to_owned),
                    if settings.as_object().unwrap().is_empty() {
                        None
                    } else {
                        Some(settings)
                    },
                )
                .await?;
            } else {
                crate::controller::start(directory, host, port).await?;
            }
        }
        "token" => {
            let args = args
                .subcommand_matches("create")
                .context("BAD_REQUEST: use controller token create")?;
            let directory = path("controller", args);
            local_files(&directory).context("CONFIG: no controller owned by this account")?;
            let database = crate::db::Db::open(&directory.join("controller.db"))?;
            let name = required(args, "name")?.to_owned();
            let role = text(args, "role").unwrap_or("admin").to_owned();
            ensure!(
                ["admin", "operator"].contains(&role.as_str()),
                "BAD_REQUEST: --role must be admin or operator"
            );
            let token = database
                .write(move |connection| {
                    crate::controller::issue_token(
                        connection, "client", &name, &role, None, None, None,
                    )
                })
                .await?;
            if flag(args, "login") {
                let config = state::read_json(&directory.join("controller.json"))?;
                let ca = fs::read_to_string(directory.join("pki/ca.pem"))?;
                let url = format!(
                    "https://127.0.0.1:{}",
                    config["port"].as_u64().unwrap_or(7300)
                );
                state::write_json(
                    &state::config_path(),
                    &json!({"version":1,"url":url,"caPem":ca,"caFingerprint":crate::pki::fingerprint(&ca)?,"token":token["token"],"tokenName":token["name"]}),
                )?;
                eprintln!(
                    "Saved local controller login to {}",
                    state::config_path().display()
                );
            } else {
                println!(
                    "{}",
                    token["token"].as_str().context("offline token value")?
                );
            }
        }
        "install-service" | "uninstall-service" => {
            return service_command("controller", action, args);
        }
        "local" => return local_command(args).await,
        "publish-build" => return publish_build(args).await,
        _ => bail!("BAD_REQUEST: unknown controller action"),
    }
    Ok(0)
}
async fn agent_command(matches: &ArgMatches) -> Result<i32> {
    let (action, args) = matches
        .subcommand()
        .context("BAD_REQUEST: choose an agent command")?;
    let directory = path("agent", args);
    match action {
        "join" => {
            if flag(args, "force") && directory.join("agent.json").exists() {
                fs::rename(
                    &directory,
                    directory.with_extension(format!("bak-{}", state::id())),
                )?;
            }
            let settings = node_settings(args)?;
            let joined = crate::worker::join(
                &directory,
                required(args, "url")?,
                required(args, "token")?,
                required(args, "fingerprint")?,
                text(args, "name").map(str::to_owned),
                if settings.as_object().unwrap().is_empty() {
                    None
                } else {
                    Some(settings)
                },
            )
            .await?;
            eprintln!(
                "Joined as {} ({})",
                joined["name"].as_str().unwrap_or("worker"),
                joined["nodeId"].as_str().unwrap_or("")
            );
        }
        "start" => {
            let settings = node_settings(args)?;
            if !settings.as_object().unwrap().is_empty() {
                let file = directory.join("agent.json");
                tokio::task::spawn_blocking(move || {
                    state::update_json(&file, |config| {
                        let mut previous = config.get("settings").cloned().unwrap_or(json!({}));
                        if let Some(reserved) = settings.get("reserved") {
                            let mut current =
                                previous.get("reserved").cloned().unwrap_or(json!({}));
                            merge(&mut current, reserved);
                            previous["reserved"] = current;
                        }
                        for (key, value) in settings.as_object().unwrap() {
                            if key != "reserved" {
                                previous[key] = value.clone();
                            }
                        }
                        config["settings"] = previous;
                        config["settingsPending"] = json!(true);
                        Ok(())
                    })
                })
                .await??;
            }
            let maximum = text(args, "cache-size")
                .map(bytes)
                .transpose()?
                .map(|bytes| bytes as u64);
            let result = crate::worker::start_options(
                directory,
                flag(args, "drain"),
                flag(args, "keep-failed"),
                maximum,
            )
            .await;
            if result
                .as_ref()
                .is_err_and(|error| error.is::<crate::worker::RestartRequested>())
            {
                return Ok(crate::install::RESTART_EXIT_CODE);
            }
            result?;
        }
        "install-service" | "uninstall-service" => return service_command("agent", action, args),
        _ => bail!("BAD_REQUEST: unknown agent action"),
    }
    Ok(0)
}
#[cfg(unix)]
fn account(name: &str) -> Result<(u32, PathBuf)> {
    let name = std::ffi::CString::new(name).context("BAD_REQUEST: invalid service account")?;
    let mut buffer = vec![0u8; 1024 * 1024];
    // Reentrant lookup owns its scratch buffer; no process-global passwd pointer escapes.
    let mut record: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            &mut record,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    ensure!(
        status == 0 && !result.is_null(),
        "BAD_REQUEST: service account does not exist"
    );
    let home = unsafe { std::ffi::CStr::from_ptr(record.pw_dir) }
        .to_str()?
        .to_owned();
    Ok((record.pw_uid, PathBuf::from(home)))
}
fn service_docker_environment(
    target_uid: u32,
    caller_uid: u32,
    environment: impl IntoIterator<Item = (String, String)>,
) -> std::collections::BTreeMap<String, String> {
    // Docker contexts and TLS/config paths belong to the account that supplied them. A system
    // service for another account must use that account's Docker configuration and credentials.
    if target_uid != caller_uid {
        return Default::default();
    }
    environment
        .into_iter()
        .filter(|(key, _)| {
            matches!(
                key.as_str(),
                "DOCKER_HOST"
                    | "DOCKER_CONTEXT"
                    | "DOCKER_CONFIG"
                    | "DOCKER_TLS_VERIFY"
                    | "DOCKER_CERT_PATH"
            )
        })
        .collect()
}
fn service_command(role: &str, action: &str, matches: &ArgMatches) -> Result<i32> {
    use crate::services::{ServiceConfig, ServiceManager, ServiceRole, ServiceScope};
    #[cfg(unix)]
    let mut uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let uid = 0;
    let mut home = dirs::home_dir().context("CONFIG: service account home unavailable")?;
    let user = if flag(matches, "system") {
        let user = text(matches, "user")
            .map(str::to_owned)
            .or_else(|| std::env::var("SUDO_USER").ok())
            .context("BAD_REQUEST: --system requires --user or SUDO_USER")?;
        #[cfg(unix)]
        {
            (uid, home) = account(&user)?;
        }
        Some(user)
    } else {
        None
    };
    let data_key = if role == "controller" {
        "GRIDSTATOR_CONTROLLER_DATA"
    } else {
        "GRIDSTATOR_AGENT_DATA"
    };
    let directory = if flag(matches, "system")
        && text(matches, "data-dir").is_none()
        && std::env::var_os(data_key).is_none()
    {
        home.join(if cfg!(target_os = "macos") {
            "Library/Application Support"
        } else {
            ".local/share"
        })
        .join("gridstator-v2")
        .join(role)
    } else {
        path(role, matches)
    };
    let directory = if directory.is_absolute() {
        directory
    } else {
        std::env::current_dir()?.join(directory)
    };
    let install_root = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| directory.join("install"));
    let store = crate::install::InstallStore::new(&install_root);
    let mut config = ServiceConfig::new(
        if role == "controller" {
            ServiceRole::Controller
        } else {
            ServiceRole::Agent
        },
        store.supervisor_path(),
        install_root,
        directory,
        home,
        uid,
    );
    #[cfg(unix)]
    let caller_uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let caller_uid = 0;
    config.environment.extend(service_docker_environment(
        uid,
        caller_uid,
        std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?))),
    ));
    if let Some(user) = user {
        config.scope = ServiceScope::System;
        config.user = Some(user);
    }
    if flag(matches, "with-agent") {
        config.extra_args.push("--with-agent".into());
        if let Some(name) = text(matches, "agent-name") {
            config
                .extra_args
                .extend(["--agent-name".into(), name.into()]);
        }
    }
    let manager = ServiceManager::current()?;
    if action == "uninstall-service" {
        manager.uninstall(&config)?;
        eprintln!("Removed isolated native {role} service");
        return Ok(0);
    }
    if flag(matches, "print") {
        print!("{}", manager.render(&config)?);
        return Ok(0);
    }
    prepare_service_installation(&config)?;
    #[cfg(unix)]
    if config.scope == ServiceScope::System && unsafe { libc::geteuid() } == 0 {
        own_service_tree(&config.data_dir, config.uid)?;
        if !config.install_root.starts_with(&config.data_dir) {
            own_service_tree(&config.install_root, config.uid)?;
        }
    }
    let installed = manager.install(&config, flag(matches, "force"))?;
    eprintln!("Installed {}", installed.display());
    Ok(0)
}

/// Both explicit CLI service installation and editor setup use the same verified payload store.
pub(crate) fn prepare_service_installation(config: &crate::services::ServiceConfig) -> Result<()> {
    let store = crate::install::InstallStore::new(&config.install_root);
    if store.state()?.is_none() {
        let executable = std::env::current_exe()?;
        let prefix = executable
            .parent()
            .and_then(Path::parent)
            .context("CONFIG: executable installation prefix unavailable")?;
        let mut selected = None;
        if let Ok(entries) = fs::read_dir(prefix.join("share/gridstator/releases")) {
            for entry in entries.filter_map(std::result::Result::ok) {
                if let Ok(manifest) = crate::release::ReleaseManifest::read(
                    entry.path().join("gridstator-release.json"),
                ) && manifest.build_id == crate::BUILD_ID
                    && manifest.version == crate::VERSION
                {
                    selected = Some(manifest);
                    break;
                }
            }
        }
        let manifest=selected.context("CONFIG: service installation needs the verified native release manifest. Install a packaged native release first; development builds support --print.")?;
        store.install_verified_executable(
            &manifest,
            crate::release::host_target()?,
            &executable,
        )?;
        store.activate(crate::BUILD_ID, None)?;
    }
    if !store.supervisor_path().exists() {
        let current = store
            .state()?
            .context("CONFIG: service installation has no active native payload")?;
        let installed = store.verify_installed(&current.current)?;
        store.install_supervisor(&installed.executable, false)?;
    }
    Ok(())
}

/// Desktop enrollment uses private bridge stdin, never a token-bearing process argument.
pub(crate) async fn join_local_worker(request: &Value) -> Result<Value> {
    let directory = request["dataDir"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| state::data_dir("agent"));
    ensure!(
        directory.is_absolute(),
        "CONFIG: local worker directory must be absolute"
    );
    if directory.exists() {
        crate::local_worker::owned_directory(&directory)?;
    } else {
        state::private_dir(&directory)?;
    }
    let endpoint = request["url"]
        .as_str()
        .or_else(|| request["controllerUrl"].as_str())
        .context("Controller URL required")?;
    let parsed = url::Url::parse(endpoint).context("Invalid HTTPS controller URL")?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.host_str().is_some()
            && parsed.username().is_empty()
            && parsed.password().is_none()
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none(),
        "Use an HTTPS controller URL without credentials, a path or a query"
    );
    let url = crate::client::normalize_url(endpoint)
        .map_err(|_| anyhow::anyhow!("Invalid HTTPS controller URL"))?;
    let fingerprint = request["fingerprint"]
        .as_str()
        .context("Controller fingerprint required")?;
    let token = request["token"]
        .as_str()
        .context("Worker join token required")?;
    ensure!(
        token.starts_with("gsj_")
            && token.len() > 4
            && token.len() <= 4096
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
        "A worker join token is required"
    );
    let service = crate::local_worker::service_config_for_install(&directory)?;
    let definition = service.definition_path(crate::services::ServicePlatform::current()?)?;
    let registered = directory.join("agent.json").exists();
    if definition.exists() {
        crate::local_worker::validate_service_definition(&service)?;
    }
    if registered {
        let saved = crate::local_worker::configuration(&directory)?;
        let expected = crate::pki::normalize_fingerprint(fingerprint)?;
        let actual =
            crate::pki::normalize_fingerprint(saved["caFingerprint"].as_str().unwrap_or(""))?;
        ensure!(
            saved["controllerUrl"] == url && expected == actual,
            "CONFLICT: this worker already belongs to another controller"
        );
        let status =
            crate::local_worker::execute(json!({"action":"status","dataDir":directory})).await?;
        if definition.exists() || status["workers"][0]["running"] == true {
            // Repeating enrollment must never restart running jobs, replace a service or change
            // existing local intent. A stopped owned service remains available through Start.
            crate::local_worker::register(&directory)?;
            return Ok(status);
        }
    }
    // Qualify the signed/checksummed native payload before consuming a one-use join token.
    let install = service.clone();
    tokio::task::spawn_blocking(move || prepare_service_installation(&install)).await??;
    if !registered {
        let name = request["name"]
            .as_str()
            .filter(|name| !name.trim().is_empty())
            .map(str::to_owned);
        crate::worker::join(&directory, &url, token, fingerprint, name, None)
            .await
            .map_err(|error| anyhow::anyhow!(format!("{error:#}").replace(token, "[redacted]")))?;
    }
    let startup = request["startAtSignIn"].as_bool().unwrap_or(true);
    crate::local_worker::execute(
        json!({"action":"setStartup","dataDir":directory,"startAtSignIn":startup}),
    )
    .await?;
    crate::local_worker::register(&directory)?;
    tokio::task::spawn_blocking(move || {
        crate::services::ServiceManager::current()?.install(&service, false)
    })
    .await??;
    crate::local_worker::set_service_startup(&directory, startup).await?;
    crate::local_worker::execute(json!({"action":"status","dataDir":directory})).await
}

#[cfg(unix)]
fn own_service_tree(path: &Path, uid: u32) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let metadata = fs::symlink_metadata(path)?;
    // Never follow a job-created or externally supplied link while assigning service ownership.
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            own_service_tree(&entry?.path(), uid)?;
        }
    }
    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    ensure!(
        unsafe { libc::lchown(path.as_ptr(), uid, u32::MAX) } == 0,
        "Cannot assign native service data to its account: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

async fn artifacts_command(matches: &ArgMatches) -> Result<i32> {
    let client = Client::load(None)?;
    let id = required(matches, "id")?;
    let response = client
        .get(&format!("/v1/jobs/{}/artifacts", encode(id)))
        .await?;
    let Some(name) = text(matches, "download") else {
        printable(&response["artifacts"], flag(matches, "json"))?;
        return Ok(0);
    };
    let artifact = response["artifacts"]
        .as_array()
        .context("Invalid artifact response")?
        .iter()
        .find(|artifact| artifact["name"] == name)
        .with_context(|| format!("NOT_FOUND: no artifact {name}"))?;
    let digest = artifact["digest"]
        .as_str()
        .context("Invalid artifact digest")?;
    ensure!(protocol::valid_digest(digest), "Invalid artifact digest");
    let digest = digest.to_owned();
    let expected = artifact["size"].as_u64().context("Invalid artifact size")?;
    let temporary = tempfile::tempdir()?;
    let store = workspace::BlobStore::new(temporary.path());
    store.init()?;
    crate::transfer::download_blobs(&client, &store, std::slice::from_ref(&digest)).await?;
    let source = store.path(&digest)?;
    ensure!(
        fs::metadata(&source)?.len() == expected,
        "Artifact size mismatch"
    );
    let destination = text(matches, "out").map(str::to_owned).unwrap_or_else(|| {
        Path::new(name)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "artifact".into())
    });
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut source = fs::File::open(source)?;
        if destination == "-" {
            std::io::copy(&mut source, &mut std::io::stdout().lock())?;
        } else {
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            std::io::copy(&mut source, &mut output)?;
            output.sync_all()?;
            eprintln!("Saved artifact to {destination}");
        }
        Ok(())
    })
    .await??;
    Ok(0)
}

async fn agents_command(matches: &ArgMatches) -> Result<i32> {
    let (action, args) = matches
        .subcommand()
        .context("BAD_REQUEST: choose agents status/update/cancel/set")?;
    let client = Client::load(None)?;
    match action {
        "status" => {
            let (view, nodes) =
                tokio::try_join!(client.get("/v1/updates"), client.get("/v1/nodes"))?;
            let nodes = nodes["nodes"]
                .as_array()
                .context("Invalid nodes response")?;
            let current = !nodes
                .iter()
                .any(|node| node["status"] == "online" && node["update"]["state"] != "current");
            let nodes=nodes.iter().map(|node|{let mut status=json!({"id":node["id"],"name":node["name"],"status":node["status"],"lastSeenAt":node["lastSeenAt"]});if node["update"].is_object(){merge(&mut status,&node["update"]);}else{merge(&mut status,&json!({"state":"unknown","build":null,"detail":"controller too old"}));}status}).collect::<Vec<_>>();
            printable(
                &json!({"controller":view["controller"],"autoUpdate":view["autoUpdate"],"updateWindow":view["updateWindow"],"current":current,"nodes":nodes,"rollout":view["rollout"]}),
                flag(args, "json"),
            )?;
            if flag(args, "check") && !current {
                return Ok(3);
            }
        }
        "update" => {
            let mut input = json!({"now":flag(args,"now"),"drainTimeoutSeconds":null});
            let nodes = values(args, "nodes");
            if !nodes.is_empty() {
                input["nodes"] = json!(nodes);
            }
            if let Some(timeout) = text(args, "drain-timeout") {
                input["drainTimeoutSeconds"] =
                    json_numeric((duration(timeout)? / 1000.0 + 0.5).floor().max(1.0));
            }
            let view = client.start_update(input).await?;
            if !flag(args, "wait") {
                output(&view)?;
                return Ok(0);
            }
            loop {
                let view = client.get("/v1/updates").await?;
                match view["rollout"]["state"].as_str() {
                    Some("running") => {}
                    Some("halted") => {
                        output(&view)?;
                        return Ok(1);
                    }
                    _ => {
                        output(&view)?;
                        return Ok(0);
                    }
                }
                tokio::select! {_=tokio::time::sleep(Duration::from_secs(2))=>{},_=tokio::signal::ctrl_c()=>{eprintln!("Detached from rollout; rollout continues on the controller.");return Ok(130);}}
            }
        }
        "cancel" => output(&client.post("/v1/updates/cancel", json!({})).await?)?,
        "set" => {
            let mut patch = json!({});
            if let Some(mode) = text(args, "auto-update") {
                ensure!(
                    ["off", "follow", "window", "inherit"].contains(&mode),
                    "BAD_REQUEST: invalid auto-update mode"
                );
                patch["autoUpdate"] = json!(mode);
            }
            if let Some(node) = text(args, "node") {
                ensure!(
                    text(args, "window").is_none() && text(args, "act").is_none(),
                    "BAD_REQUEST: update window and managed act are cluster settings"
                );
                ensure!(
                    !patch.as_object().unwrap().is_empty(),
                    "BAD_REQUEST: Nothing to change"
                );
                output(
                    &client
                        .request(
                            reqwest::Method::PATCH,
                            &format!("/v1/nodes/{}", encode(node)),
                            Some(patch),
                        )
                        .await?,
                )?;
            } else {
                ensure!(
                    patch["autoUpdate"] != "inherit",
                    "BAD_REQUEST: inherit applies only to an individual worker"
                );
                if let Some(window) = text(args, "window") {
                    let regex = regex::Regex::new(r"^(\d{2}:\d{2})-(\d{2}:\d{2})$")?;
                    let captures = regex
                        .captures(window.trim())
                        .context("BAD_REQUEST: update window must be HH:MM-HH:MM")?;
                    patch["updateWindow"] = json!({"start":&captures[1],"end":&captures[2]});
                }
                if let Some(version) = text(args, "act") {
                    let current = client.get("/v1/settings").await?;
                    let mut toolchain = current["toolchain"].clone();
                    if !toolchain.is_object() {
                        toolchain = json!({});
                    }
                    toolchain.as_object_mut().unwrap().remove("node");
                    toolchain["act"] = if version == "pinned" {
                        Value::Null
                    } else {
                        json!(version)
                    };
                    patch["toolchain"] = toolchain;
                }
                ensure!(
                    !patch.as_object().unwrap().is_empty(),
                    "BAD_REQUEST: Nothing to change"
                );
                output(
                    &client
                        .request(reqwest::Method::PATCH, "/v1/settings", Some(patch))
                        .await?,
                )?;
            }
        }
        _ => bail!("BAD_REQUEST: unknown agents command"),
    }
    Ok(0)
}
fn local_files(directory: &Path) -> Result<(Value, String, String)> {
    let database = directory.join("controller.db");
    let config_file = directory.join("controller.json");
    for file in [&database, &config_file] {
        let metadata = fs::metadata(file)?;
        ensure!(
            metadata.is_file(),
            "FORBIDDEN: local controller file is not regular"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            ensure!(
                metadata.uid() == unsafe { libc::geteuid() },
                "FORBIDDEN: this account does not own the local controller"
            );
        }
    }
    let config = state::read_json(&config_file)?;
    ensure!(
        config["version"] == 2,
        "FORBIDDEN: refusing non-v2 controller data"
    );
    let ca = fs::read_to_string(directory.join("pki/ca.pem"))?;
    let fingerprint = crate::pki::fingerprint(&ca)?;
    Ok((config, ca, fingerprint))
}
fn local_url(host: &str, port: u64) -> String {
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "[::1]",
        host if host.contains(':') && !host.starts_with('[') => {
            return format!("https://[{host}]:{port}");
        }
        host => host,
    };
    format!("https://{host}:{port}")
}
fn local_service_config(directory: &Path) -> Result<crate::services::ServiceConfig> {
    let directory = if directory.is_absolute() {
        directory.to_owned()
    } else {
        std::env::current_dir()?.join(directory)
    };
    let root = std::env::var_os("GRIDSTATOR_INSTALL_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| directory.join("install"));
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let uid = 0;
    Ok(crate::services::ServiceConfig::new(
        crate::services::ServiceRole::Controller,
        root.join("supervisor"),
        root,
        directory,
        dirs::home_dir().context("CONFIG: service home unavailable")?,
        uid,
    ))
}
pub(crate) fn service_words(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        match character {
            '\\' => {
                let next = chars.next()?;
                if next == 'x' {
                    let hex = String::from_iter([chars.next()?, chars.next()?]);
                    word.push(char::from(u8::from_str_radix(&hex, 16).ok()?));
                } else {
                    word.push(match next {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        other => other,
                    });
                }
            }
            character if quote == Some(character) => quote = None,
            character if quote.is_none() && (character == '"' || character == '\'') => {
                quote = Some(character)
            }
            character if quote.is_none() && character.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            character => word.push(character),
        }
    }
    if quote.is_some() {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    Some(words)
}
#[cfg(target_os = "macos")]
fn service_owns(tokens: &[String], config: &crate::services::ServiceConfig) -> bool {
    service_owns_environment(tokens, config, None, false)
}
#[cfg(any(test, target_os = "macos"))]
fn service_owns_environment(
    tokens: &[String],
    config: &crate::services::ServiceConfig,
    environment: Option<&str>,
    has_environment_files: bool,
) -> bool {
    let normalize = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    service_directory(tokens, config, environment, has_environment_files)
        .is_some_and(|directory| normalize(&directory) == normalize(&config.data_dir))
}
fn service_directory(
    tokens: &[String],
    config: &crate::services::ServiceConfig,
    environment: Option<&str>,
    has_environment_files: bool,
) -> Option<PathBuf> {
    let normalize = |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let executable = tokens.first()?;
    let arguments = if tokens
        .get(1)
        .is_some_and(|argument| argument == "supervise")
    {
        if normalize(Path::new(executable)) != normalize(&config.supervisor) {
            return None;
        }
        let separator = tokens.iter().position(|argument| argument == "--")?;
        &tokens[separator + 1..]
    } else {
        let path = Path::new(executable);
        if path.file_name().is_none_or(|name| name != "gridstator")
            || crate::release::host_target().ok().is_none_or(|target| {
                crate::release::verify_executable_target(path, target).is_err()
            })
        {
            return None;
        }
        &tokens[1..]
    };
    if arguments
        .first()
        .is_none_or(|argument| argument != "controller")
        || arguments.get(1).is_none_or(|argument| argument != "start")
        || arguments
            .iter()
            .any(|argument| argument == "-c" || argument == "--eval")
    {
        return None;
    }
    let explicit = arguments
        .iter()
        .rposition(|argument| argument == "--data-dir" || argument.starts_with("--data-dir="));
    let directory = if let Some(index) = explicit {
        let argument = &arguments[index];
        let value = if argument == "--data-dir" {
            arguments.get(index + 1).map(String::as_str)
        } else {
            argument.strip_prefix("--data-dir=")
        };
        let value = value?;
        PathBuf::from(value)
    } else {
        if has_environment_files {
            return None;
        }
        let words = service_words(environment.unwrap_or(""))?;
        let variables = words
            .iter()
            .filter_map(|word| word.split_once('='))
            .collect::<std::collections::HashMap<_, _>>();
        if let Some(directory) = variables.get("GRIDSTATOR_CONTROLLER_DATA") {
            PathBuf::from(directory)
        } else if let Some(directory) = variables.get("XDG_DATA_HOME") {
            Path::new(directory).join("gridstator-v2/controller")
        } else {
            let home = variables
                .get("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| config.home.clone());
            home.join(if cfg!(target_os = "macos") {
                "Library/Application Support/gridstator-v2/controller"
            } else {
                ".local/share/gridstator-v2/controller"
            })
        }
    };
    directory.is_absolute().then_some(directory)
}

async fn local_service_view(config: &crate::services::ServiceConfig) -> Value {
    let mut view = json!({"installed":false,"state":"unknown","scope":"user","canManage":false,"reason":"No native controller user service is installed."});
    #[cfg(target_os = "linux")]
    {
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::process::Command::new("systemctl")
                .args([
                    "--user",
                    "show",
                    &config.unit_name(),
                    "--property=LoadState",
                    "--property=ActiveState",
                    "--property=ExecStart",
                    "--property=Environment",
                    "--property=EnvironmentFiles",
                ])
                .output(),
        )
        .await;
        if let Ok(Ok(output)) = output
            && output.status.success()
        {
            let text = String::from_utf8_lossy(&output.stdout);
            let properties = text
                .lines()
                .filter_map(|line| line.split_once('='))
                .collect::<std::collections::HashMap<_, _>>();
            view["installed"] = json!(properties.get("LoadState") == Some(&"loaded"));
            let state = properties.get("ActiveState").copied().unwrap_or("unknown");
            view["state"] = json!(if ["active", "inactive", "failed"].contains(&state) {
                state
            } else {
                "unknown"
            });
            if view["installed"] == true {
                let tokens = properties
                    .get("ExecStart")
                    .and_then(|command| {
                        command
                            .split_once("argv[]=")
                            .map(|(_, tail)| tail.split(" ;").next().unwrap_or(tail))
                    })
                    .and_then(service_words);
                let directory = tokens.as_ref().and_then(|tokens| {
                    service_directory(
                        tokens,
                        config,
                        properties.get("Environment").copied(),
                        properties
                            .get("EnvironmentFiles")
                            .is_some_and(|files| !files.trim().is_empty()),
                    )
                });
                let normalize =
                    |path: &Path| fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
                let owned = directory
                    .as_ref()
                    .is_some_and(|directory| normalize(directory) == normalize(&config.data_dir));
                view["canManage"] = json!(owned);
                view["reason"] = if owned {
                    Value::Null
                } else {
                    json!(directory.map_or_else(|| "Cannot verify which controller data directory the installed service uses. Install it with an explicit --data-dir before managing it here.".into(), |directory| format!("The installed controller service uses another data directory ({}). Host controls for {} are disabled.", directory.display(),config.data_dir.display())))
                };
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let path = config.definition_path(crate::services::ServicePlatform::Launchd);
        if let Ok(path) = path
            && let Ok(text) = fs::read_to_string(path)
        {
            let mut reader = quick_xml::Reader::from_str(&text);
            let mut program = false;
            let mut tokens = Vec::new();
            loop {
                match reader.read_event() {
                    Ok(quick_xml::events::Event::Start(tag)) if tag.name().as_ref() == "key" => {
                        if let Ok(key) = reader.read_text(tag.name()) {
                            program = key.as_ref() == "ProgramArguments";
                        }
                    }
                    Ok(quick_xml::events::Event::Start(tag))
                        if program && tag.name().as_ref() == "string" =>
                    {
                        if let Ok(value) = reader.read_text(tag.name())
                            && let Ok(value) = quick_xml::escape::unescape(&value)
                        {
                            tokens.push(value.into_owned());
                        }
                    }
                    Ok(quick_xml::events::Event::End(tag))
                        if program && tag.name().as_ref() == "array" =>
                    {
                        break;
                    }
                    Ok(quick_xml::events::Event::Eof) | Err(_) => break,
                    _ => {}
                }
            }
            let owned = service_owns(&tokens, config);
            view["installed"] = json!(true);
            view["canManage"] = json!(owned);
            view["reason"] = if owned {
                Value::Null
            } else {
                json!("The native launchd agent does not run this controller data directory.")
            };
            let target = format!("gui/{}/{}", config.uid, config.label);
            if let Ok(Ok(output)) = tokio::time::timeout(
                Duration::from_secs(20),
                tokio::process::Command::new("launchctl")
                    .args(["print", &target])
                    .output(),
            )
            .await
            {
                let output_text = String::from_utf8_lossy(&output.stdout);
                view["state"] = json!(if output.status.success()
                    && output_text.contains("state = running")
                {
                    "active"
                } else {
                    "inactive"
                });
            }
        }
    }
    view
}
fn non_loopback_url(input: &str) -> Option<String> {
    let normalized = crate::client::normalize_url(input).ok()?;
    let parsed = url::Url::parse(&normalized).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host == "localhost"
        || host.ends_with(".localhost")
        || matches!(host.as_str(), "[::1]" | "[::]" | "0.0.0.0")
        || host.starts_with("127.")
    {
        None
    } else {
        Some(normalized)
    }
}
fn lan_address() -> Option<String> {
    #[cfg(unix)]
    {
        struct Interfaces(*mut libc::ifaddrs);
        impl Drop for Interfaces {
            fn drop(&mut self) {
                // getifaddrs owns every linked-list node until freeifaddrs.
                unsafe { libc::freeifaddrs(self.0) };
            }
        }
        let mut head = std::ptr::null_mut();
        // Enumerate in the OS order used by Node's os.networkInterfaces().
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return None;
        }
        let _interfaces = Interfaces(head);
        let mut current = head;
        while !current.is_null() {
            // The allocation remains alive for this traversal and each AF_INET address has
            // sockaddr_in layout. No returned reference outlives the list.
            let interface = unsafe { &*current };
            if !interface.ifa_addr.is_null()
                && interface.ifa_flags & libc::IFF_LOOPBACK as u32 == 0
                && i32::from(unsafe { (*interface.ifa_addr).sa_family }) == libc::AF_INET
            {
                let address = unsafe { &*interface.ifa_addr.cast::<libc::sockaddr_in>() };
                return Some(
                    std::net::Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()).to_string(),
                );
            }
            current = interface.ifa_next;
        }
    }
    None
}
async fn local_status(directory: &Path, config_path: &Path) -> Result<Value> {
    let config = local_service_config(directory)?;
    let service = local_service_view(&config).await;
    let pending = owned_setup_record(&directory.join("setup-pending.json"));
    let mut result = json!({"available":false,"dataDir":directory,"hostname":sysinfo::System::host_name().unwrap_or_default(),"clusterId":null,"caFingerprint":null,"host":"0.0.0.0","port":7300,"url":"https://127.0.0.1:7300","aliases":[],"service":service,"setupPending":pending.as_ref().is_some_and(|pending| pending["initializing"]==true)});
    if let Ok((local, _, fingerprint)) = local_files(directory) {
        let host = local["host"].as_str().unwrap_or("0.0.0.0");
        let port = local["port"].as_u64().unwrap_or(7300);
        result["available"] = json!(true);
        result["clusterId"] = local["clusterId"].clone();
        result["setupPending"] = json!(pending.as_ref().is_some_and(|pending| {
            pending["initializing"] == true || pending["clusterId"] == local["clusterId"]
        }));
        if let Some(persistence) =
            owned_setup_record(&directory.join("setup-service-persistence.json"))
            && persistence["clusterId"] == local["clusterId"]
            && let Some(warning) = persistence["warning"].as_str()
            && !user_linger_enabled().await
        {
            result["service"]["persistenceWarning"] = json!(warning);
        }
        result["caFingerprint"] = json!(fingerprint);
        result["host"] = json!(host);
        result["port"] = json!(port);
        let saved = state::read_json(config_path).ok();
        result["url"] = json!(
            local["publicUrl"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| {
                    saved
                        .as_ref()
                        .filter(|saved| saved["caFingerprint"] == fingerprint)
                        .and_then(|saved| saved["url"].as_str())
                        .and_then(non_loopback_url)
                })
                .unwrap_or_else(|| local_url(lan_address().as_deref().unwrap_or(host), port))
        );
        result["aliases"] = local.get("aliases").cloned().unwrap_or(json!([]));
    }
    Ok(result)
}

fn owned_setup_record(path: &Path) -> Option<Value> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return None;
        }
    }
    state::read_json(path).ok()
}

fn archive_setup_initialization(directory: &Path) -> Result<()> {
    let mut artifacts = Vec::new();
    for name in [
        "controller.json",
        "controller.db",
        "controller.db-wal",
        "controller.db-shm",
        "pki",
    ] {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    if name == "pki" {
                        metadata.file_type().is_dir()
                    } else {
                        metadata.file_type().is_file()
                    },
                    "CONFIG: refusing to recover unexpected controller setup artifacts"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    ensure!(
                        metadata.uid() == unsafe { libc::geteuid() },
                        "FORBIDDEN: this account does not own incomplete controller setup data"
                    );
                }
                artifacts.push(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !artifacts.is_empty() {
        let recovery = directory.join(format!("setup-recovery-{}", state::id()));
        state::private_dir(&recovery)?;
        for artifact in artifacts {
            fs::rename(&artifact, recovery.join(artifact.file_name().unwrap()))?;
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
async fn user_linger_enabled() -> bool {
    tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("loginctl")
            .args(["show-user", "--property=Linger", "--value"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .is_some_and(|output| {
        output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "yes"
    })
}

#[cfg(not(target_os = "linux"))]
async fn user_linger_enabled() -> bool {
    true
}

#[cfg(target_os = "linux")]
async fn enable_setup_linger() -> Option<&'static str> {
    let enabled = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("loginctl")
            .args(["--no-ask-password", "enable-linger"])
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()
    .and_then(std::result::Result::ok)
    .is_some_and(|output| output.status.success());
    if enabled || user_linger_enabled().await {
        None
    } else {
        Some(
            "The controller is running, but Linux could not enable user-service persistence. It may stop after the last SSH session closes. Run `loginctl enable-linger` as this account, or ask an administrator to enable lingering for this account.",
        )
    }
}

#[cfg(not(target_os = "linux"))]
async fn enable_setup_linger() -> Option<&'static str> {
    None
}
async fn local_lock(path: &Path) -> Result<fs::File> {
    use fs2::FileExt;
    let parent = path.parent().context("CONFIG: lock path needs a parent")?;
    state::private_dir(parent)?;
    let mut options = fs::OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(path)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        match lock.try_lock_exclusive() {
            Ok(()) => return Ok(lock),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                ensure!(
                    tokio::time::Instant::now() < deadline,
                    "CONFLICT: another window is changing local controller access"
                );
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}
async fn ensure_local_login(directory: &Path, config_path: &Path, restore: bool) -> Result<Value> {
    let lock_path = config_path.with_extension("local-login.lock");
    let _lock = local_lock(&lock_path).await?;
    let (local, ca, fingerprint) = match local_files(directory) {
        Ok(files) => files,
        Err(_) => {
            return Ok(
                json!({"status":"unavailable","configPath":config_path,"message":"No controller owned by this account was found"}),
            );
        }
    };
    let saved: Option<crate::client::Config> = if config_path.exists() {
        Some(
            serde_json::from_slice(&fs::read(config_path)?)
                .context("CONFIG: invalid saved client configuration")?,
        )
    } else {
        None
    };
    if saved
        .as_ref()
        .is_some_and(|saved| saved.ca_fingerprint != fingerprint)
    {
        return Ok(
            json!({"status":"differentCluster","configPath":config_path,"message":"The saved connection selects another cluster"}),
        );
    }
    let database = crate::db::Db::open(&directory.join("controller.db"))?;
    if let Some(saved) = &saved {
        use sha2::Digest;
        let hash = hex::encode(sha2::Sha256::digest(saved.token.as_bytes()));
        let token = database
            .call(move |connection| {
                Ok(crate::db::list(connection, "tokens")?
                    .into_iter()
                    .find(|token| token["hash"] == hash))
            })
            .await?;
        let valid = token.as_ref().is_some_and(|token| {
            token["kind"] == "client"
                && token["revokedAt"].is_null()
                && token["expiresAt"]
                    .as_i64()
                    .is_none_or(|expires| expires >= state::now())
        });
        if !valid && !restore {
            return Ok(
                json!({"status":"revoked","configPath":config_path,"url":saved.url,"message":"Local access was revoked or expired. Use Restore Local Access to reconnect."}),
            );
        }
        if valid && token.as_ref().is_some_and(|token| token["role"] == "admin") {
            return Ok(
                json!({"status":"connected","configPath":config_path,"url":saved.url,"role":"admin"}),
            );
        }
    }
    let name = format!(
        "vscode:{}",
        std::env::var("USER").unwrap_or_else(|_| "local".into())
    );
    let token_name = name.clone();
    let issued = database
        .write(move |connection| {
            crate::controller::issue_token(connection, "client", &name, "admin", None, None, None)
        })
        .await?;
    let url = saved
        .as_ref()
        .map(|saved| saved.url.clone())
        .unwrap_or_else(|| {
            local_url(
                local["host"].as_str().unwrap_or("0.0.0.0"),
                local["port"].as_u64().unwrap_or(7300),
            )
        });
    state::write_json(
        config_path,
        &json!({"version":1,"url":url,"caPem":ca,"caFingerprint":fingerprint,"token":issued["token"],"tokenName":token_name}),
    )?;
    Ok(json!({"status":"connected","configPath":config_path,"url":url,"role":"admin"}))
}
async fn local_client(directory: &Path, config_path: &Path) -> Result<Client> {
    let login = ensure_local_login(directory, config_path, false).await?;
    ensure!(
        login["status"] == "connected",
        "FORBIDDEN: {}",
        login["message"]
            .as_str()
            .unwrap_or("Restore local controller access first")
    );
    let mut config: crate::client::Config = serde_json::from_slice(&fs::read(config_path)?)?;
    let (local, _, _) = local_files(directory)?;
    config.url = local_url(
        local["host"].as_str().unwrap_or("0.0.0.0"),
        local["port"].as_u64().unwrap_or(7300),
    );
    Client::new(config)
}
async fn protect_embedded(directory: &Path, config_path: &Path) -> Result<Option<(String, Value)>> {
    let state_file = directory.join("embedded-agent/agent.json");
    if !state_file.exists() {
        return Ok(None);
    }
    let embedded = state::read_json(&state_file)?;
    let Some(id) = embedded["nodeId"].as_str() else {
        return Ok(None);
    };
    let client = local_client(directory, config_path).await?;
    let node = client.get(&format!("/v1/nodes/{}", encode(id))).await?;
    let original = node["settings"]["schedulingMode"].clone();
    let updated = client
        .request(
            reqwest::Method::PATCH,
            &format!("/v1/nodes/{}", encode(id)),
            Some(json!({"schedulingMode":"draining"})),
        )
        .await?;
    if updated["activeJobs"]
        .as_array()
        .is_some_and(|jobs| !jobs.is_empty())
    {
        client
            .request(
                reqwest::Method::PATCH,
                &format!("/v1/nodes/{}", encode(id)),
                Some(json!({"schedulingMode":original})),
            )
            .await?;
        bail!(
            "CONFLICT: Finish the {} running job(s) on {} before restarting the controller\nhint: The controller also runs this worker. No jobs were cancelled.",
            updated["activeJobs"].as_array().unwrap().len(),
            node["name"].as_str().unwrap_or("embedded")
        );
    }
    Ok(Some((id.into(), original)))
}
async fn restore_embedded(directory: &Path, previous: Option<(String, Value)>) -> Result<()> {
    if let Some((id, mode)) = previous {
        let database = crate::db::Db::open(&directory.join("controller.db"))?;
        database
            .write(move |connection| {
                if let Some(mut node) = crate::db::get(connection, "nodes", &id)? {
                    node["settings"]["schedulingMode"] = mode;
                    crate::db::put(connection, "nodes", &id, &node)?;
                }
                Ok(())
            })
            .await?;
    }
    Ok(())
}
async fn control_local(
    config: crate::services::ServiceConfig,
    action: crate::services::ServiceAction,
) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        crate::services::ServiceManager::current()?.control(&config, action)
    })
    .await??;
    Ok(())
}

/// Explicit editor onboarding. Existing connections and controller identities are never replaced,
/// and the response deliberately excludes the credentials written to the shared client file.
pub(crate) async fn setup_local_controller(config_path: &Path, port: Option<u16>) -> Result<Value> {
    use crate::services::ServiceManager;
    ensure!(
        config_path.is_absolute(),
        "CONFIG: controller setup requires an absolute client configuration path"
    );
    ensure!(
        ["GRIDSTATOR_URL", "GRIDSTATOR_TOKEN", "GRIDSTATOR_CA_FILE"]
            .iter()
            .all(|key| std::env::var_os(key).is_none()),
        "CONFIG: an environment connection is configured; use that connection or remove its overrides before local setup"
    );
    let manager = ServiceManager::current()?;
    let mut service = local_service_config(&state::data_dir("controller"))?;
    let directory = service.data_dir.clone();
    let _setup_lock = local_lock(&directory.join("setup.lock")).await?;
    let status = local_status(&directory, config_path).await?;
    let installed = status["service"]["installed"] == true;
    let pending_path = directory.join("setup-pending.json");
    let pending = owned_setup_record(&pending_path);
    let initializing = pending.as_ref().is_some_and(|pending| {
        pending["initializing"] == true
            && pending["configPath"]
                .as_str()
                .is_none_or(|path| Path::new(path) == config_path)
    });
    let resuming = initializing
        || pending.as_ref().is_some_and(|pending| {
            status["clusterId"].is_string() && pending["clusterId"] == status["clusterId"]
        });
    ensure!(
        !installed || status["service"]["canManage"] == true,
        "FORBIDDEN: the installed controller service belongs to another controller"
    );
    if installed {
        ensure!(
            status["service"]["state"] == "active" || resuming,
            "CONFIG: this controller service is stopped; use Start Controller to resume it"
        );
    }
    {
        // This is the same login lock used by automatic sign-in in other editor windows.
        let _login_lock = local_lock(&config_path.with_extension("local-login.lock")).await?;
        let existing = local_files(&directory).ok();
        let saved: Option<crate::client::Config> = if config_path.exists() {
            Some(
                serde_json::from_slice(&fs::read(config_path)?)
                    .map_err(|_| anyhow::anyhow!("CONFIG: invalid saved client configuration"))?,
            )
        } else {
            None
        };
        ensure!(
            saved.as_ref().is_none_or(|saved| existing
                .as_ref()
                .is_some_and(|(_, _, fingerprint)| saved.ca_fingerprint == *fingerprint)),
            "CONFIG: the saved connection selects another cluster; reconnect to it instead of creating a controller"
        );
        if existing.is_none() {
            let artifacts_exist = [
                "controller.json",
                "controller.db",
                "controller.db-wal",
                "controller.db-shm",
                "pki",
            ]
            .iter()
            .any(|file| fs::symlink_metadata(directory.join(file)).is_ok());
            ensure!(
                !artifacts_exist || (initializing && saved.is_none() && !installed),
                "CONFIG: incomplete or unowned controller data exists; repair that controller before setup"
            );
            if artifacts_exist {
                archive_setup_initialization(&directory)?;
            }
            // Check the packaged release before initializing a cluster. A development executable
            // cannot provide the verified managed payload needed by a persistent service.
            let install = service.clone();
            tokio::task::spawn_blocking(move || prepare_service_installation(&install)).await??;
            let port = match port {
                Some(port) => port,
                None => available_setup_port(7300)?,
            };
            // This durable ownership record precedes the first PKI/config/database mutation.
            // Cancellation can then recover only this setup's incomplete artifacts on retry.
            state::write_json(
                &pending_path,
                &json!({"initializing":true,"configPath":config_path}),
            )?;
            let initialized =
                crate::controller::initialize(&directory, "0.0.0.0", port, &[], false, config_path)
                    .await?;
            state::write_json(
                &pending_path,
                &json!({"clusterId":initialized["clusterId"]}),
            )?;
        } else if let Some((local, _, _)) = &existing
            && initializing
        {
            // Initialization may have finished just before cancellation. Preserve that cluster
            // and any saved credentials instead of generating a replacement identity.
            state::write_json(&pending_path, &json!({"clusterId":local["clusterId"]}))?;
        }
    }
    let login = ensure_local_login(&directory, config_path, false).await?;
    ensure!(
        login["status"] == "connected",
        "CONFIG: {}",
        login["message"]
            .as_str()
            .unwrap_or("local controller access could not be established")
    );
    if !installed {
        let (local, _, _) = local_files(&directory)?;
        state::write_json(&pending_path, &json!({"clusterId":local["clusterId"]}))?;
        service.extra_args.push("--with-agent".into());
        service.environment.extend(service_docker_environment(
            service.uid,
            service.uid,
            std::env::vars_os().filter_map(|(key, value)| {
                Some((key.into_string().ok()?, value.into_string().ok()?))
            }),
        ));
        tokio::task::spawn_blocking(move || -> Result<()> {
            prepare_service_installation(&service)?;
            let definition =
                service.definition_path(crate::services::ServicePlatform::current()?)?;
            // Installation may previously have written the definition and then failed to contact
            // the service manager. Only our exact definition can be retried without --force.
            let retry = fs::symlink_metadata(&definition)
                .ok()
                .is_some_and(|metadata| metadata.file_type().is_file())
                && fs::read(&definition).ok().as_deref()
                    == Some(manager.render(&service)?.as_bytes());
            manager.install(&service, retry)?;
            Ok(())
        })
        .await??;
    } else if status["service"]["state"] != "active" {
        control_local(service, crate::services::ServiceAction::Start).await?;
    }
    let mut saved: crate::client::Config = serde_json::from_slice(&fs::read(config_path)?)
        .map_err(|_| anyhow::anyhow!("CONFIG: invalid saved client configuration"))?;
    let (local, _, _) = local_files(&directory)?;
    saved.url = local_url(
        local["host"].as_str().unwrap_or("0.0.0.0"),
        local["port"].as_u64().unwrap_or(7300),
    );
    let client = Client::new(saved)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(Ok(cluster)) =
            tokio::time::timeout(Duration::from_secs(2), client.get("/v1/cluster")).await
            && cluster["clusterId"] == local["clusterId"]
        {
            break;
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "CONFIG: the controller service was installed but did not become ready; check its service log and use Start Controller or Retry"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    let persistence_warning = enable_setup_linger().await;
    let persistence_path = directory.join("setup-service-persistence.json");
    if let Some(warning) = persistence_warning {
        state::write_json(
            &persistence_path,
            &json!({"clusterId":local["clusterId"],"warning":warning}),
        )?;
    } else if owned_setup_record(&persistence_path)
        .is_some_and(|record| record["clusterId"] == local["clusterId"])
    {
        fs::remove_file(persistence_path)?;
    }
    if resuming || !installed {
        fs::remove_file(pending_path)?;
    }
    let waiting = crate::worker::node_info()
        .await
        .ok()
        .is_some_and(|info| info["docker"]["available"] != true);
    let mut result = json!({
        "status":"connected",
        "host":local_status(&directory, config_path).await?,
        "workerWaitingForDocker":waiting,
    });
    if let Some(warning) = persistence_warning {
        result["persistenceWarning"] = json!(warning);
    }
    Ok(result)
}

fn available_setup_port(preferred: u16) -> Result<u16> {
    let listener = match std::net::TcpListener::bind(("0.0.0.0", preferred)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            std::net::TcpListener::bind(("0.0.0.0", 0))?
        }
        Err(error) => return Err(error.into()),
    };
    Ok(listener.local_addr()?.port())
}

async fn local_command(matches: &ArgMatches) -> Result<i32> {
    let (action, args) = matches
        .subcommand()
        .context("BAD_REQUEST: choose a local controller operation")?;
    let directory = path("controller", args);
    let config_path = text(args, "config")
        .map(PathBuf::from)
        .unwrap_or_else(state::config_path);
    if action == "status" {
        output(&local_status(&directory, &config_path).await?)?;
        return Ok(0);
    }
    if action == "ensure-login" {
        output(&ensure_local_login(&directory, &config_path, flag(args, "restore")).await?)?;
        return Ok(0);
    }
    let _lock = local_lock(&directory.join("host-operation.lock")).await?;
    let status = local_status(&directory, &config_path).await?;
    ensure!(
        status["available"] == true,
        "FORBIDDEN: this account does not own a local controller"
    );
    ensure!(
        status["service"]["installed"] == true,
        "CONFIG: install a native controller user service first"
    );
    ensure!(
        status["service"]["canManage"] == true,
        "FORBIDDEN: {}",
        status["service"]["reason"]
            .as_str()
            .unwrap_or("The installed service does not run this controller")
    );
    if action == "service" {
        use crate::services::ServiceAction;
        let action = match required(args, "action")? {
            "start" => ServiceAction::Start,
            "stop" => ServiceAction::Stop,
            "restart" => ServiceAction::Restart,
            _ => bail!("BAD_REQUEST: service action must be start, stop or restart"),
        };
        let previous = if action != ServiceAction::Start && status["service"]["state"] == "active" {
            protect_embedded(&directory, &config_path).await?
        } else {
            None
        };
        let result = control_local(local_service_config(&directory)?, action).await;
        let restore = restore_embedded(&directory, previous).await;
        result?;
        restore?;
        output(&local_status(&directory, &config_path).await?)?;
        return Ok(0);
    }
    ensure!(
        action == "configure",
        "BAD_REQUEST: unknown local controller operation"
    );
    let host = required(args, "host")?;
    let port = required(args, "port")?
        .parse::<u16>()
        .context("BAD_REQUEST: port must be 1..65535")?;
    ensure!(
        port > 0
            && !host.is_empty()
            && !host.chars().any(|character| character.is_whitespace()
                || character == '/'
                || character == '\\'),
        "BAD_REQUEST: invalid listening address/port"
    );
    let url = crate::client::normalize_url(required(args, "url")?)?;
    let parsed = url::Url::parse(&url)?;
    ensure!(
        parsed.port_or_known_default() == Some(port),
        "BAD_REQUEST: public URL must use the configured listening port"
    );
    ensure!(
        !matches!(parsed.host_str(), Some("0.0.0.0" | "[::]" | "::")),
        "BAD_REQUEST: workers need a reachable URL, not a wildcard"
    );
    let (before, ca, _) = local_files(&directory)?;
    let coexist = |a: &Value, b: &Value| {
        a["port"] != b["port"]
            || (a["host"] != b["host"]
                && !matches!(a["host"].as_str(), Some("0.0.0.0" | "::"))
                && !matches!(b["host"].as_str(), Some("0.0.0.0" | "::")))
    };
    let mut next = before.clone();
    next["host"] = json!(host);
    next["port"] = json!(port);
    next["publicUrl"] = json!(url);
    let mut aliases = before["aliases"].as_array().cloned().unwrap_or_default();
    let old = json!({"host":before["host"],"port":before["port"]});
    if (old["host"] != next["host"] || old["port"] != next["port"]) && !aliases.contains(&old) {
        aliases.push(old);
    }
    aliases.retain(|alias| alias["host"] != next["host"] || alias["port"] != next["port"]);
    ensure!(
        aliases.iter().all(|alias| coexist(alias, &next)),
        "CONFLICT: choose a different port so old worker endpoints can remain available"
    );
    next["aliases"] = json!(aliases);
    let client = local_client(&directory, &config_path).await?;
    let workers = if status["service"]["state"] == "active" {
        client.get("/v1/nodes").await?["nodes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let previous = if status["service"]["state"] == "active" {
        protect_embedded(&directory, &config_path).await?
    } else {
        None
    };
    let saved = state::read_json(&config_path)?;
    state::write_json(&directory.join("controller.json"), &next)?;
    let result = async {
        control_local(
            local_service_config(&directory)?,
            crate::services::ServiceAction::Restart,
        )
        .await?;
        let mut verify_config = client.config.clone();
        verify_config.url = url.clone();
        verify_config.ca_pem = ca;
        let verify = Client::new(verify_config)?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
        loop {
            if let Ok(cluster) = verify.get("/v1/cluster").await
                && cluster["clusterId"] == before["clusterId"]
                && let Ok(current) = verify.get("/v1/nodes").await
            {
                let nodes = current["nodes"].as_array().cloned().unwrap_or_default();
                let reconnected = workers
                    .iter()
                    .filter(|worker| {
                        worker["status"] == "online"
                            && worker["info"].get("controllerUrl").is_some()
                    })
                    .all(|worker| {
                        nodes.iter().any(|node| {
                            node["id"] == worker["id"]
                                && node["status"] == "online"
                                && node["info"]["controllerUrl"] == url
                        })
                    });
                if reconnected {
                    break;
                }
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "UNAVAILABLE: controller or workers did not reconnect at the new address"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let mut saved = saved.clone();
        saved["url"] = json!(local_url(host, port.into()));
        state::write_json(&config_path, &saved)?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = result {
        state::write_json(&directory.join("controller.json"), &before)?;
        let _ = control_local(
            local_service_config(&directory)?,
            crate::services::ServiceAction::Restart,
        )
        .await;
        let _ = restore_embedded(&directory, previous).await;
        bail!(
            "UNAVAILABLE: The address change failed and the previous configuration was restored: {error:#}"
        );
    }
    restore_embedded(&directory, previous).await?;
    output(&local_status(&directory, &config_path).await?)?;
    Ok(0)
}
async fn publish_build(matches: &ArgMatches) -> Result<i32> {
    let path=text(matches,"tarball").map(PathBuf::from).or_else(||std::env::var_os("GRIDSTATOR_RELEASE_BUNDLE").map(PathBuf::from)).context("BAD_REQUEST: supply the native release bundle path: controller publish-build <bundle.tar.gz>")?;
    let source = path.clone();
    let (size, sha) =
        tokio::task::spawn_blocking(move || crate::release::hash_file(source)).await??;
    ensure!(
        size <= 512 * 1024 * 1024,
        "BAD_REQUEST: native release bundle exceeds 512 MiB"
    );
    let client = Client::load(None)?;
    let file = tokio::fs::File::open(path).await?;
    let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));
    let response = client
        .http
        .post(format!(
            "{}/v1/controller/build?sha256={sha}",
            client.config.url
        ))
        .bearer_auth(&client.config.token)
        .header("content-type", "application/gzip")
        .header("content-length", size)
        .body(body)
        .send()
        .await?;
    let status = response.status();
    let value: Value = response.json().await?;
    ensure!(
        status.is_success(),
        "{}: {}",
        status.as_u16(),
        value["error"]["message"]
            .as_str()
            .unwrap_or("native release publication failed")
    );
    output(&value)?;
    if value["action"] == "none" || flag(matches, "no-wait") {
        return Ok(0);
    }
    let target = value["to"]
        .as_str()
        .context("publish response target missing")?;
    loop {
        if let Ok(cluster) = client.get("/v1/cluster").await {
            if cluster["build"]["id"] == target {
                eprintln!("Controller now runs {target}");
                return Ok(0);
            }
            ensure!(
                cluster["build"]["lastRollback"]["from"] != target,
                "UNAVAILABLE: controller candidate rolled back; the previous executable and database were restored"
            );
        }
        if let Ok(status) = client.get("/v1/updates").await {
            ensure!(
                status["controllerUpdate"]["phase"] != "failed",
                "UNAVAILABLE: {}",
                status["controllerUpdate"]["error"]
                    .as_str()
                    .unwrap_or("controller update failed")
            );
        }
        tokio::select! {_=tokio::time::sleep(Duration::from_secs(1))=>{},_=tokio::signal::ctrl_c()=>{eprintln!("Detached from controller update; the accepted update continues.");return Ok(130);}}
    }
}
fn file_mode(path: &Path) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
async fn doctor_command(matches: &ArgMatches) -> Result<i32> {
    let mut checks = vec![
        json!({"name":"native executable","status":"ok","detail":format!("v{} ({})",crate::VERSION,crate::release::host_target()? )}),
    ];
    let git = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("git")
            .arg("--version")
            .output(),
    )
    .await;
    if let Ok(Ok(result)) = git {
        if result.status.success() {
            checks.push(json!({"name":"git","status":"ok","detail":String::from_utf8_lossy(&result.stdout).trim()}));
        } else {
            checks.push(json!({"name":"git","status":"warn","detail":"not found","hint":"Without git, snapshots use a .gitignore-aware directory walk."}));
        }
    } else {
        checks.push(json!({"name":"git","status":"warn","detail":"not found","hint":"Without git, snapshots use a .gitignore-aware directory walk."}));
    }
    let config_path = state::config_path();
    if config_path.exists() {
        let valid = fs::read(&config_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<crate::client::Config>(&bytes).ok())
            .is_some();
        let permissions = file_mode(&config_path);
        checks.push(json!({"name":"client config","status":if !valid{"fail"}else if permissions.is_some_and(|mode|mode&0o077!=0){"warn"}else{"ok"},"detail":config_path}));
    } else {
        checks.push(json!({"name":"client config","status":if std::env::var_os("GRIDSTATOR_URL").is_some(){"info"}else{"warn"},"detail":"not logged in","hint":"Run gridstator login <url> --fingerprint ... --token ..."}));
    }
    let connection=async{
        let client=Client::load(None)?;let health=client.get("/v1/health").await?;let mut result=vec![json!({"name":"controller","status":"ok","detail":format!("{} (v{}, TLS verified against pinned CA)",client.config.url,health["version"].as_str().unwrap_or("?"))})];let cluster=client.get("/v1/cluster").await?;result.push(json!({"name":"authentication","status":"ok","detail":format!("signed in as {} ({})",cluster["whoami"]["name"].as_str().unwrap_or("?"),cluster["whoami"]["role"].as_str().unwrap_or("?"))}));let nodes=client.get("/v1/nodes").await?;let nodes=nodes["nodes"].as_array().context("Invalid nodes response")?;let online=nodes.iter().filter(|node|node["status"]=="online").count();result.push(json!({"name":"nodes","status":if online>0{"ok"}else{"warn"},"detail":format!("{online}/{} online",nodes.len())}));let behind=nodes.iter().filter(|node|node["update"].is_object()&&node["update"]["state"]!="current"&&node["update"]["state"]!="unknown").count();result.push(json!({"name":"worker versions","status":if behind==0{"ok"}else{"warn"},"detail":format!("{behind} reachable worker(s) differ from controller build {}",cluster["build"]["id"].as_str().unwrap_or("?"))}));
        for node in nodes.iter().filter(|node|node["status"]=="online"){let status=if node["health"]=="degraded"{"warn"}else if node["settings"]["schedulingMode"]!="enabled"{"info"}else{"ok"};result.push(json!({"name":format!("node {}",node["name"].as_str().unwrap_or("?")),"status":status,"detail":if status=="warn"{array_strings(&node["healthIssues"]).join("; ")}else{format!("scheduling {}; {} active job(s)",node["settings"]["schedulingMode"].as_str().unwrap_or("?"),node["activeJobs"].as_array().map_or(0,Vec::len))}}));}Ok::<_,anyhow::Error>(result)
    }.await;
    match connection {
        Ok(result) => checks.extend(result),
        Err(error) => {
            checks.push(json!({"name":"controller","status":"fail","detail":format!("{error:#}")}))
        }
    }
    let info = crate::worker::node_info().await?;
    checks.push(json!({"name":"local docker","status":if info["docker"]["available"]==true{"ok"}else{"info"},"detail":if info["docker"]["available"]==true{info["docker"]["version"].clone()}else{info["docker"].get("error").cloned().unwrap_or(json!("unavailable"))},"hint":"Docker is needed when this machine runs a worker."}));
    if let Ok(agent) = state::read_json(&state::data_dir("agent").join("agent.json")) {
        checks.push(json!({"name":"local agent","status":"info","detail":format!("joined as {} → {}",agent["name"].as_str().unwrap_or("?"),agent["controllerUrl"].as_str().unwrap_or("?"))}));
    }
    let directory = state::data_dir("controller");
    if let Some(mode) = file_mode(&directory) {
        checks.push(json!({"name":"local controller data","status":if mode&0o077==0{"ok"}else{"warn"},"detail":directory}));
    }
    let cwd = std::env::current_dir()?;
    match tokio::task::spawn_blocking(move || locate(&cwd)).await? {
        Ok(project) => {
            let config = ["gridstator.yml", "gridstator.yaml"]
                .into_iter()
                .map(|name| project.root.join(name))
                .find(|path| path.exists());
            checks.push(json!({"name":if config.is_some(){"project config"}else{"project"},"status":if config.is_some(){"ok"}else{"info"},"detail":config.unwrap_or(project.root)}));
        }
        Err(error) => checks
            .push(json!({"name":"project config","status":"fail","detail":format!("{error:#}")})),
    }
    if flag(matches, "json") {
        output(&json!(checks))?;
    } else {
        for check in &checks {
            println!(
                "{} {:22} {}",
                match check["status"].as_str() {
                    Some("ok") => "✓",
                    Some("warn") => "!",
                    Some("fail") => "✗",
                    _ => "i",
                },
                check["name"].as_str().unwrap_or(""),
                check["detail"].as_str().unwrap_or("")
            );
            if check["status"] != "ok"
                && let Some(hint) = check["hint"].as_str()
            {
                println!("  {hint}");
            }
        }
    }
    Ok(if checks.iter().any(|check| check["status"] == "fail") {
        1
    } else {
        0
    })
}
async fn group_reports(client: &Client, group: &Value) -> Result<Value> {
    let mut sets = Vec::new();
    for job in group["jobs"].as_array().into_iter().flatten() {
        let Some(artifacts) = job["artifacts"].as_array() else {
            continue;
        };
        let mut cases = Vec::new();
        for artifact in crate::reports::candidate_reports(artifacts) {
            let parsed = async {
                let digest = artifact["digest"]
                    .as_str()
                    .context("Invalid report digest")?;
                let expected = artifact["size"].as_u64().context("Invalid report size")?;
                let mut response = client
                    .http
                    .post(format!("{}/v1/blobs/download", client.config.url))
                    .bearer_auth(&client.config.token)
                    .json(&json!({"digests":[digest]}))
                    .send()
                    .await?
                    .error_for_status()?;
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await? {
                    ensure!(
                        bytes.len() as u64 + chunk.len() as u64 <= expected + 1024 * 1024,
                        "Report response exceeds its declared size"
                    );
                    bytes.extend_from_slice(&chunk);
                }
                let name = artifact["name"]
                    .as_str()
                    .context("Report name missing")?
                    .to_owned();
                let digest = digest.to_owned();
                tokio::task::spawn_blocking(move || {
                    let temporary = tempfile::tempdir()?;
                    let store = workspace::BlobStore::new(temporary.path());
                    store.init()?;
                    workspace::unpack_blobs(
                        bytes.as_slice(),
                        &store,
                        workspace::UnpackLimits {
                            max_entries: Some(1),
                            max_bytes: Some(expected),
                        },
                    )?;
                    let bytes = store.read(&digest)?;
                    ensure!(bytes.len() as u64 == expected, "Report size mismatch");
                    crate::reports::collect_junit(&name, &bytes)
                })
                .await?
            }
            .await;
            if let Ok(parsed) = parsed {
                cases.extend(parsed.as_array().into_iter().flatten().cloned());
            }
        }
        if !cases.is_empty() {
            let mut set = json!({"cases":cases});
            if let Some(shard) = job.get("shardIndex").filter(|shard| !shard.is_null()) {
                set["shard"] = shard.clone();
            }
            sets.push(set);
        }
    }
    if sets.is_empty() {
        Ok(Value::Null)
    } else {
        crate::reports::summarize(&sets)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_setup_selects_an_available_port_without_changing_existing_bindings() -> Result<()> {
        let occupied = std::net::TcpListener::bind(("0.0.0.0", 0))?;
        let port = occupied.local_addr()?.port();
        let available = available_setup_port(port)?;
        assert_ne!(available, port);
        assert!(std::net::TcpListener::bind(("0.0.0.0", available)).is_ok());
        assert_eq!(occupied.local_addr()?.port(), port);
        Ok(())
    }
    #[test]
    fn completed_build_and_actions_outputs_preserve_source_image_and_artifact_guidance() {
        let mut job = json!({"id":"job","kind":"docker-build","state":"succeeded","nodeName":"beta","outputs":{"image":{"id":"sha256:image","tags":["example:test","other:tag"],"sizeBytes":6144}},"artifacts":[{"name":"image.tar","size":2048}]});
        assert_eq!(
            workload_output_lines(&job),
            [
                "Image sha256:image (example:test, other:tag), 6.0 KiB on beta",
                "Artifact image.tar (2.0 KiB): gridstator artifacts job --download image.tar"
            ]
        );
        job["state"] = json!("failed");
        assert_eq!(
            workload_output_lines(&job),
            ["Artifact image.tar (2.0 KiB): gridstator artifacts job --download image.tar"]
        );
        job["kind"] = json!("actions");
        job["state"] = json!("succeeded");
        job["outputs"]["image"]["tags"] = json!([]);
        job["outputs"]["image"]["sizeBytes"] = Value::Null;
        job["artifacts"] = json!([]);
        assert_eq!(workload_output_lines(&job), ["Image sha256:image on beta"]);
        assert_eq!(output_bytes(10.5 * 1024.0), "11 KiB");
        assert_eq!(output_bytes(1.25 * 1024.0), "1.3 KiB");
        assert_eq!(
            terminal_summary_lines(
                &json!({"id":"job","state":"failed","nodeName":"beta","startedAt":1000,"finishedAt":2250,"exitCode":7,"error":"failure"})
            ),
            ["Job job failed on beta in 1.3s (exit code 7)", "  failure"]
        );
        assert_eq!(
            terminal_summary_lines(
                &json!({"id":"lost","state":"lost","nodeName":null,"startedAt":null,"exitCode":null,"error":"worker went away"})
            ),
            [
                "Job lost lost",
                "  worker went away",
                "  Retry with: gridstator retry lost"
            ]
        );
        assert_eq!(output_duration(1150.0), "1.1s");
        assert_eq!(output_duration(60000.0), "1m00s");
        assert_eq!(output_duration(3600000.0), "1h00m");
    }
    #[tokio::test]
    async fn command_workflows_units_settings_and_job_exits_match_frozen_source() {
        let fixture: Value =
            serde_json::from_str(include_str!("../contracts/cli-workflows.json")).unwrap();
        let mut failures = vec![];
        for case in fixture["cases"].as_array().unwrap() {
            let args = case["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|arg| arg.as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            let parse::Parsed::Arguments(arguments) = parse::arguments(&args, catalog()) else {
                panic!("fixture parser failed {}", case["id"])
            };
            let matches = command()
                .try_get_matches_from(std::iter::once("gridstator".to_owned()).chain(arguments))
                .unwrap();
            let result = async {
                let project = Project {
                    root: PathBuf::from("/project-fixture"),
                    workdir: case["workdir"].as_str().unwrap().into(),
                    config: discovery::normalize_project_config(case["config"].clone())
                        .context("CONFIG: invalid project")?,
                };
                let (mut spec, shards) = prepare_workload(
                    "command",
                    matches.subcommand_matches("run").unwrap(),
                    &project,
                    &|name| case["environment"][name].as_str().map(str::to_owned),
                )
                .await?;
                if let Some(snapshot) = case.get("snapshot") {
                    spec["snapshot"] = snapshot.clone();
                }
                Ok::<_, anyhow::Error>((protocol::normalize_spec(spec)?, shards))
            }
            .await;
            match result {
                Ok((spec, shards)) => {
                    if case["success"] != true
                        || spec != case["output"]
                        || Some(shards) != case["shards"].as_u64()
                    {
                        failures.push(format!("{} normalized spec/shards differs", case["id"]));
                    }
                }
                Err(error) => {
                    if case["success"] == true
                        || Some(i64::from(error_code(&error))) != case["exitCode"].as_i64()
                    {
                        failures.push(format!("{} error/exit differs: {error:#}", case["id"]));
                    }
                }
            }
        }
        for case in fixture["units"].as_array().unwrap() {
            let input = case["input"].as_str().unwrap();
            let result = match case["fn"].as_str().unwrap() {
                "bytes" => bytes(input),
                "megabytes" => megabytes(&json!(input)),
                _ => duration(input),
            };
            match result {
                Ok(output) => {
                    if case["success"] != true || Some(output) != case["output"].as_f64() {
                        failures.push(format!("unit {} {:?} differs", case["fn"], input));
                    }
                }
                Err(_) => {
                    if case["success"] == true {
                        failures.push(format!("unit {} {:?} rejected", case["fn"], input));
                    }
                }
            }
        }
        for case in fixture["settings"].as_array().unwrap() {
            let mut argv = vec![
                "gridstator".to_owned(),
                "nodes".into(),
                "set".into(),
                "worker".into(),
            ];
            for (key, value) in case["flags"].as_object().unwrap() {
                let flag = match key.as_str() {
                    "reserveCpus" => "reserve-cpus",
                    "maxConcurrency" => "max-concurrency",
                    _ => "labels",
                };
                argv.push(format!("--{flag}={}", value.as_str().unwrap()));
            }
            let matches = command().try_get_matches_from(argv).unwrap();
            let args = matches
                .subcommand_matches("nodes")
                .unwrap()
                .subcommand_matches("set")
                .unwrap();
            let result = node_settings(args);
            if case["success"] == true {
                if result.ok().as_ref() != Some(&case["output"]) {
                    failures.push(format!("settings {} differs", case["flags"]));
                }
            } else if result.is_ok() {
                failures.push(format!("settings {} accepted", case["flags"]));
            }
        }
        for case in fixture["exits"].as_array().unwrap() {
            assert_eq!(
                i64::from(exit_for_job(case)),
                case["output"].as_i64().unwrap()
            );
        }
        assert!(
            failures.is_empty(),
            "{} workflow differences:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
    #[test]
    fn errors_preserve_frozen_code_and_http_status_exit_mappings() {
        let reference: Value =
            serde_json::from_str(include_str!("../contracts/behavior/errors-and-units.json"))
                .unwrap();
        for (code, mapping) in reference["errors"].as_object().unwrap() {
            let expected = mapping["exitCode"].as_i64().unwrap() as i32;
            assert_eq!(
                error_code(&anyhow::anyhow!("{code}: reference failure")),
                expected,
                "{code}"
            );
            let status = mapping["status"].as_u64().unwrap();
            let remote = anyhow::Error::new(crate::client::ControllerError {
                status: status as u16,
                code: Some(code.clone()),
                message: "reference failure".into(),
                hint: None,
                details: None,
                cause: None,
            })
            .context("CLI request failed");
            assert_eq!(error_code(&remote), expected, "typed {code}");
            assert_eq!(render_failure(&remote), "error: reference failure\n");
            // HTTP500 requires the structured error code to distinguish CONFIG from INTERNAL.
            if code != "CONFIG" {
                assert_eq!(
                    error_code(&anyhow::anyhow!("{status}: reference failure")),
                    expected,
                    "HTTP{status}"
                );
            }
        }
    }
    #[test]
    fn remote_error_presentation_preserves_source_hint_and_omission() {
        let source: Value =
            serde_json::from_str(include_str!("../contracts/behavior/errors-and-units.json"))
                .unwrap();
        for case in source["bodies"].as_array().unwrap() {
            let body = &case["body"]["error"];
            let error = anyhow::Error::new(crate::client::ControllerError {
                status: case["status"].as_u64().unwrap() as u16,
                code: body["code"].as_str().map(str::to_owned),
                message: body["message"].as_str().unwrap().into(),
                hint: body["hint"].as_str().map(str::to_owned),
                details: body.get("details").cloned(),
                cause: None,
            });
            let mut expected = format!("error: {}\n", body["message"].as_str().unwrap());
            if let Some(hint) = body["hint"].as_str().filter(|hint| !hint.is_empty()) {
                expected.push_str(&format!("hint: {hint}\n"));
            }
            assert_eq!(render_failure(&error), expected);
            assert_eq!(
                i64::from(error_code(&error)),
                case["exitCode"].as_i64().unwrap()
            );
        }
    }
    #[test]
    fn service_preserves_same_account_docker_connection_and_excludes_foreign_credentials() {
        let environment = [
            ("DOCKER_HOST", "unix:///private/docker.sock"),
            ("DOCKER_CONTEXT", "desktop-linux"),
            ("DOCKER_CONFIG", "/home/alice/docker config"),
            ("DOCKER_TLS_VERIFY", "1"),
            ("DOCKER_CERT_PATH", "/home/alice/certs"),
            ("HOME", "/home/alice"),
            ("AWS_SECRET_ACCESS_KEY", "not-a-docker-setting"),
        ]
        .map(|(key, value)| (key.to_owned(), value.to_owned()));
        let inherited = service_docker_environment(1000, 1000, environment.clone());
        assert_eq!(inherited.len(), 5);
        assert_eq!(inherited["DOCKER_CONTEXT"], "desktop-linux");
        assert_eq!(inherited["DOCKER_HOST"], "unix:///private/docker.sock");
        assert!(!inherited.contains_key("HOME"));
        assert!(service_docker_environment(1001, 1000, environment).is_empty());
        let directory = tempfile::tempdir().unwrap();
        let mut config = local_service_config(directory.path()).unwrap();
        config.environment.extend(inherited);
        let rendered = crate::services::ServiceManager::current()
            .unwrap()
            .render(&config)
            .unwrap();
        assert!(rendered.contains("DOCKER_CONTEXT"));
        assert!(rendered.contains("desktop-linux"));
        assert!(rendered.contains("unix:///private/docker.sock"));
    }
    #[test]
    fn scalar_flags_take_the_last_value_and_collection_flags_append() {
        let matches = command()
            .try_get_matches_from([
                "gridstator",
                "run",
                "--image",
                "first",
                "--image",
                "last",
                "--env",
                "A=1",
                "--env",
                "B=2",
                "--",
                "true",
            ])
            .unwrap();
        let args = matches.subcommand_matches("run").unwrap();
        assert_eq!(text(args, "image"), Some("last"));
        assert_eq!(values(args, "env"), vec!["A=1", "B=2"]);
    }
    #[test]
    fn service_identity_uses_explicit_or_verified_environment_paths_and_refuses_wrappers() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("controller with spaces");
        fs::create_dir_all(&directory).unwrap();
        let config = local_service_config(&directory).unwrap();
        let arguments = vec![
            config.supervisor.to_string_lossy().into_owned(),
            "supervise".into(),
            "--install-root".into(),
            config.install_root.to_string_lossy().into_owned(),
            "--".into(),
            "controller".into(),
            "start".into(),
        ];
        let environment = format!(
            "GRIDSTATOR_LOG_FORMAT=json {}",
            serde_json::to_string(&format!(
                "GRIDSTATOR_CONTROLLER_DATA={}",
                directory.display()
            ))
            .unwrap()
        );
        assert!(service_owns_environment(
            &arguments,
            &config,
            Some(&environment),
            false
        ));
        assert!(!service_owns_environment(
            &arguments,
            &config,
            Some(&environment),
            true
        ));
        let mut explicit = arguments.clone();
        explicit.push(format!("--data-dir={}", directory.display()));
        assert!(service_owns_environment(&explicit, &config, None, true));
        assert!(!service_owns_environment(
            &[
                "/bin/bash".into(),
                "-c".into(),
                "gridstator controller start".into()
            ],
            &config,
            Some(&environment),
            false
        ));
        let mut wrong = arguments;
        *wrong.last_mut().unwrap() = "stop".into();
        assert!(!service_owns_environment(
            &wrong,
            &config,
            Some(&environment),
            false
        ));
    }
}
