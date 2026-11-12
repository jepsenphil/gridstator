//! Project discovery mirrors the retained TypeScript discovery contracts.
use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::fs;
use std::path::Path;

fn strings(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    js_string(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

pub fn analyze_job(id: &str, job: &Value) -> Value {
    let runner = &job["runs-on"];
    let mut runs_on = strings(Some(runner));
    if runner.is_object() {
        runs_on = strings(runner.get("labels"));
        if let Some(group) = runner["group"].as_str() {
            runs_on.push(format!("group:{group}"));
        }
    }
    let mut warnings = Vec::<String>::new();
    let unsupported = Regex::new("(?i)windows|macos|mac-os|osx").expect("runner regex");
    let runnable = !runs_on.iter().any(|label| unsupported.is_match(label));
    if !runnable {
        warnings.push(format!(
            "runs-on {}: act runs Linux containers only, so this job cannot run",
            runs_on.join(", ")
        ));
    } else if runs_on.iter().any(|label| label == "self-hosted") {
        warnings.push(
            "self-hosted runner labels need an image mapping (actions.platforms in gridstator.yml)"
                .into(),
        );
    } else if runs_on.iter().any(|label| label.contains("${{")) {
        warnings.push(
            "runs-on is an expression; act needs it to resolve to a label with an image mapping"
                .into(),
        );
    }
    let uses = job["uses"].as_str();
    if let Some(uses) = uses.filter(|uses| !uses.is_empty() && !uses.starts_with("./")) {
        warnings.push(format!("calls remote reusable workflow {uses}; act fetches it from GitHub (network and possibly a GITHUB_TOKEN secret needed)"));
    }
    if job.get("environment").is_some() {
        warnings.push("deployment environments and their protection rules and secrets are not available locally".into());
    }
    if job["permissions"]["id-token"] == "write" {
        warnings.push("OIDC tokens (id-token: write) are not available locally".into());
    }
    let docker_action = Regex::new(r"^docker/(build-push-action|setup-buildx-action|login-action|bake-action|metadata-action)@").expect("action regex");
    let docker_command = Regex::new(r"(^|[\s;&|])docker\s").expect("command regex");
    let mut needs_docker = false;
    if let Some(steps) = job["steps"].as_array() {
        for step in steps.iter().filter(|step| step.is_object()) {
            let action = step["uses"].as_str().unwrap_or("");
            if action.starts_with("actions/checkout@")
                && let Some(repository) = step["with"]["repository"]
                    .as_str()
                    .filter(|repository| !repository.contains("github.repository"))
            {
                warnings.push(format!("actions/checkout of {repository} clones that repository from GitHub (your snapshot is used only for this repository)"));
            }
            needs_docker |= docker_action.is_match(action)
                || step["run"]
                    .as_str()
                    .is_some_and(|run| docker_command.is_match(run));
        }
    }
    if needs_docker {
        warnings.push("uses Docker inside the job; enable the Docker socket (actions.dockerSocket or --docker-socket)".into());
    }
    let mut result = json!({"id":id,"name":job["name"].as_str().unwrap_or(id),"runsOn":runs_on,"needs":strings(job.get("needs")),"matrix":job["strategy"].is_object() && job["strategy"].get("matrix").is_some(),"runnable":runnable,"warnings":warnings});
    if let Some(uses) = uses.filter(|uses| !uses.is_empty()) {
        result["usesWorkflow"] = json!(uses);
    }
    result
}

pub fn parse_workflow(file: &str, text: &str) -> Value {
    let empty = |error: String| json!({"file":file,"name":file,"events":[],"defaultEvent":"push","dispatchInputs":[],"jobs":[],"warnings":[],"parseError":error});
    let doc: Value = match serde_yaml_ng::from_str(text) {
        Ok(doc) => doc,
        Err(error) => return empty(error.to_string()),
    };
    if !doc.is_object() {
        return empty("not a workflow (expected a mapping)".into());
    }
    let on = doc
        .get("on")
        .filter(|value| !value.is_null())
        .or_else(|| doc.get("true"));
    let events: Vec<String> = match on {
        Some(Value::Object(object)) => object.keys().cloned().collect(),
        other => strings(other),
    };
    let jobs: Vec<Value> = doc["jobs"]
        .as_object()
        .into_iter()
        .flat_map(|jobs| jobs.iter())
        .filter(|(_, job)| job.is_object())
        .map(|(id, job)| analyze_job(id, job))
        .collect();
    let mut warnings = Vec::<String>::new();
    if text.contains("secrets.GITHUB_TOKEN") || text.contains("github.token") {
        warnings.push("uses GITHUB_TOKEN; locally it is only set if you forward one (--secret-env GITHUB_TOKEN)".into());
    }
    if doc["permissions"]["id-token"] == "write" {
        warnings.push("OIDC tokens (id-token: write) are not available locally".into());
    }
    if jobs.is_empty() {
        warnings.push("no jobs found".into());
    }
    let default_event = if events.iter().any(|event| event == "push") {
        "push"
    } else if events.iter().any(|event| event == "workflow_dispatch") {
        "workflow_dispatch"
    } else {
        events.first().map(String::as_str).unwrap_or("push")
    };
    let mut inputs = Vec::new();
    if let Some(definitions) = on
        .and_then(|on| on.get("workflow_dispatch"))
        .and_then(|dispatch| dispatch.get("inputs"))
        .and_then(Value::as_object)
    {
        for (name, definition) in definitions {
            let mut input = json!({"name":name,"required":definition["required"] == true});
            for key in ["description", "type"] {
                if definition[key].is_string() {
                    input[key] = definition[key].clone();
                }
            }
            if let Some(value) = definition
                .get("default")
                .filter(|value| value.is_string() || value.is_number() || value.is_boolean())
            {
                input["default"] = json!(js_string(value));
            }
            if let Some(options) = definition["options"].as_array() {
                input["options"] = json!(options.iter().map(js_string).collect::<Vec<_>>());
            }
            inputs.push(input);
        }
    }
    json!({"file":file,"name":doc["name"].as_str().unwrap_or_else(|| file.rsplit('/').next().unwrap_or(file)),"events":events,"defaultEvent":default_event,"dispatchInputs":inputs,"jobs":jobs,"warnings":warnings})
}

fn utf16_sort(values: &mut [String]) {
    values.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
}

pub fn discover_workflows(root: &Path) -> Result<Vec<Value>> {
    let directory = root.join(".github/workflows");
    let Ok(entries) = fs::read_dir(&directory) else {
        return Ok(Vec::new());
    };
    let mut names = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".yml") || name.ends_with(".yaml"))
        .collect::<Vec<_>>();
    utf16_sort(&mut names);
    names
        .into_iter()
        .map(|name| {
            let bytes = fs::read(directory.join(&name))?;
            Ok(parse_workflow(
                &format!(".github/workflows/{name}"),
                &String::from_utf8_lossy(&bytes),
            ))
        })
        .collect()
}

pub fn find_workflow<'a>(workflows: &'a [Value], query: &str) -> Option<&'a Value> {
    let clean = query.strip_prefix("./").unwrap_or(query);
    workflows
        .iter()
        .find(|workflow| workflow["file"] == query || workflow["file"] == clean)
        .or_else(|| {
            workflows.iter().find(|workflow| {
                workflow["file"]
                    .as_str()
                    .is_some_and(|file| file.rsplit('/').next() == Some(query))
            })
        })
        .or_else(|| workflows.iter().find(|workflow| workflow["name"] == query))
}

fn walk_files(root: &Path, max_depth: usize, matches: impl Fn(&str) -> bool) -> Vec<String> {
    fn visit(
        root: &Path,
        relative: &str,
        depth: usize,
        max_depth: usize,
        matches: &impl Fn(&str) -> bool,
        out: &mut Vec<String>,
    ) {
        let Ok(entries) = fs::read_dir(root.join(relative)) else {
            return;
        };
        for entry in entries.filter_map(std::result::Result::ok) {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let path = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if kind.is_dir() {
                if depth < max_depth
                    && ![
                        "node_modules",
                        ".git",
                        "dist",
                        "build",
                        "coverage",
                        ".next",
                        ".turbo",
                        "target",
                        "vendor",
                    ]
                    .contains(&name.as_str())
                    && (!name.starts_with('.') || name == ".github")
                {
                    visit(root, &path, depth + 1, max_depth, matches, out);
                }
            } else if kind.is_file() && matches(&name) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    visit(root, "", 0, max_depth, &matches, &mut out);
    utf16_sort(&mut out);
    out
}

pub fn parse_dockerfile(text: &str) -> Value {
    let from =
        Regex::new(r"(?i)^\s*FROM\s+(?:--\S+\s+)*(\S+)(?:\s+AS\s+(\S+))?\s*$").expect("FROM regex");
    let mut stages = Vec::<String>::new();
    let mut base_images = Vec::<String>::new();
    for line in text.lines() {
        if let Some(found) = from.captures(line) {
            base_images.push(found[1].into());
            if let Some(stage) = found.get(2) {
                stages.push(stage.as_str().into());
            }
        }
    }
    json!({"stages":stages,"baseImages":base_images})
}

pub fn discover_dockerfiles(root: &Path) -> Result<Vec<Value>> {
    let pattern = Regex::new(r"^Dockerfile(?:\..+)?$|\.[Dd]ockerfile$").expect("Dockerfile regex");
    Ok(walk_files(root, 4, |name| pattern.is_match(name))
        .into_iter()
        .map(|file| {
            let text = fs::read(root.join(&file))
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_default();
            let mut parsed = parse_dockerfile(&text);
            parsed["context"] = json!(
                file.rsplit_once('/')
                    .map_or(".", |(directory, _)| directory)
            );
            parsed["file"] = json!(file);
            parsed
        })
        .collect())
}

fn package_manager(root: &Path, directory: &str, declared: Option<&Value>) -> &'static str {
    if let Some(name) = declared
        .and_then(Value::as_str)
        .and_then(|value| value.split('@').next())
    {
        match name {
            "pnpm" => return "pnpm",
            "npm" => return "npm",
            "yarn" => return "yarn",
            "bun" => return "bun",
            _ => {}
        }
    }
    for directory in if directory == "." {
        vec!["."]
    } else {
        vec![directory, "."]
    } {
        let directory = root.join(directory);
        for (manager, locks) in [
            ("pnpm", vec!["pnpm-lock.yaml"]),
            ("yarn", vec!["yarn.lock"]),
            ("bun", vec!["bun.lockb", "bun.lock"]),
            ("npm", vec!["package-lock.json"]),
        ] {
            if locks.iter().any(|lock| directory.join(lock).exists()) {
                return manager;
            }
        }
    }
    "npm"
}

pub fn discover_package_scripts(root: &Path) -> Result<Vec<Value>> {
    let root_package = match fs::read(root.join("package.json")) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes).context("parse root package.json")?,
        Err(_) => Value::Null,
    };
    let mut packages = Vec::new();
    for file in walk_files(root, 3, |name| name == "package.json") {
        let Ok(bytes) = fs::read(root.join(&file)) else {
            continue;
        };
        let Ok(package) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        let scripts: Map<String, Value> = package["scripts"]
            .as_object()
            .into_iter()
            .flat_map(|scripts| scripts.iter())
            .filter(|(_, value)| value.is_string())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        if scripts.is_empty() {
            continue;
        }
        let directory = file
            .rsplit_once('/')
            .map_or(".", |(directory, _)| directory);
        let declared = package
            .get("packageManager")
            .filter(|value| !value.is_null())
            .or_else(|| root_package.get("packageManager"));
        packages.push(json!({"dir":directory,"name":package["name"].as_str().unwrap_or(directory),"packageManager":package_manager(root,directory,declared),"scripts":scripts}));
    }
    Ok(packages)
}

pub fn script_command(manager: &str, script: &str, directory: &str) -> Result<String> {
    let cd = if directory == "." {
        String::new()
    } else {
        format!(" && cd {}", serde_json::to_string(directory)?)
    };
    let script = serde_json::to_string(script)?;
    Ok(match manager {
        "pnpm" => {
            format!("corepack enable && pnpm install --frozen-lockfile{cd} && pnpm run {script}")
        }
        "yarn" => format!("corepack enable && yarn install --immutable{cd} && yarn run {script}"),
        "bun" => format!("bun install --frozen-lockfile{cd} && bun run {script}"),
        "npm" => format!("npm ci{cd} && npm run {script}"),
        _ => bail!("Unsupported package manager {manager}"),
    })
}

fn strict(value: &Value, allowed: &[&str], field: &str) -> Result<()> {
    let object = value
        .as_object()
        .with_context(|| format!("{field}: expected object"))?;
    for key in object.keys() {
        ensure!(
            allowed.contains(&key.as_str()),
            "{field}: unknown field {key}"
        );
    }
    Ok(())
}
fn optional_strings(value: &Value, fields: &[&str]) -> Result<()> {
    for field in fields {
        if let Some(value) = value.get(*field) {
            ensure!(value.is_string(), "{field}: expected string");
        }
    }
    Ok(())
}
fn string_array(value: &Value, field: &str) -> Result<()> {
    ensure!(
        value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string)),
        "{field}: expected string array"
    );
    Ok(())
}
fn optional_arrays(value: &Value, fields: &[&str]) -> Result<()> {
    for field in fields {
        if let Some(value) = value.get(*field) {
            string_array(value, field)?;
        }
    }
    Ok(())
}
fn string_record(value: &Value, field: &str, env_names: bool) -> Result<()> {
    let values = value
        .as_object()
        .with_context(|| format!("{field}: expected string map"))?;
    let env_name = Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("env regex");
    for (name, value) in values {
        ensure!(value.is_string(), "{field}.{name}: expected string");
        if env_names {
            ensure!(
                env_name.is_match(name) && value.as_str().unwrap().chars().count() <= 128 * 1024,
                "{field}.{name}: invalid environment variable"
            );
        }
    }
    Ok(())
}
const MAX_SAFE_PROJECT_INTEGER: u64 = 9_007_199_254_740_991;
fn size(value: &Value) -> bool {
    value
        .as_u64()
        .is_some_and(|size| (1..=MAX_SAFE_PROJECT_INTEGER).contains(&size))
        || value.as_str().is_some_and(|size| !size.is_empty())
}
fn bounded_integer(value: &Value, min: i64, max: i64) -> bool {
    value
        .as_i64()
        .is_some_and(|number| (min..=max).contains(&number))
}
fn validate_resources(value: &Value) -> Result<()> {
    strict(value, &["cpus", "memory"], "resources")?;
    if let Some(cpus) = value.get("cpus") {
        ensure!(
            cpus.as_f64().is_some_and(|cpus| cpus > 0.0),
            "resources.cpus: expected positive number"
        );
    }
    if let Some(memory) = value.get("memory") {
        ensure!(
            size(memory),
            "resources.memory: expected positive MiB integer or size string"
        );
    }
    Ok(())
}
fn labels(value: &Value) -> Result<()> {
    string_array(value, "labels")?;
    let pattern =
        Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*(=[A-Za-z0-9._:/-]+)?$").expect("label regex");
    for label in value.as_array().unwrap() {
        let label = label.as_str().unwrap();
        ensure!(
            label.len() <= 128 && pattern.is_match(label),
            "invalid label"
        );
    }
    Ok(())
}

// JavaScript YAML parsers use one Number type: `2.0` and `2` reach Zod as the
// same integer. Normalize exactly representable whole floats before validating or
// returning config so downstream integer accessors preserve those settings too.
fn normalize_project_numbers(value: &mut Value) {
    match value {
        Value::Number(number) if number.is_f64() => {
            if let Some(number) = number.as_f64().filter(|number| {
                number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= MAX_SAFE_PROJECT_INTEGER as f64
            }) {
                *value = Value::from(number as i64);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(normalize_project_numbers),
        Value::Object(fields) => fields.values_mut().for_each(normalize_project_numbers),
        _ => {}
    }
}

pub fn normalize_project_config(mut config: Value) -> Result<Value> {
    normalize_project_numbers(&mut config);
    strict(
        &config,
        &[
            "version", "project", "defaults", "snapshot", "tasks", "builds", "actions",
        ],
        "project",
    )?;
    if let Some(version) = config.get("version") {
        ensure!(version.as_f64() == Some(1.0), "version: expected 1");
    }
    optional_strings(&config, &["project"])?;
    let defaults = config.get("defaults").cloned().unwrap_or(json!({}));
    strict(
        &defaults,
        &["image", "resources", "labels", "prefer", "timeout"],
        "defaults",
    )?;
    optional_strings(&defaults, &["image", "timeout"])?;
    optional_arrays(&defaults, &["prefer"])?;
    if let Some(value) = defaults.get("resources") {
        validate_resources(value)?;
    }
    if let Some(value) = defaults.get("labels") {
        labels(value)?;
    }
    let mut snapshot = config.get("snapshot").cloned().unwrap_or(json!({}));
    strict(
        &snapshot,
        &["include", "exclude", "allowSecrets", "maxFileSize"],
        "snapshot",
    )?;
    for field in ["include", "exclude", "allowSecrets"] {
        if snapshot.get(field).is_none() {
            snapshot[field] = json!([]);
        }
        string_array(&snapshot[field], field)?;
    }
    if snapshot.get("maxFileSize").is_none() {
        snapshot["maxFileSize"] = json!("256m");
    }
    ensure!(
        size(&snapshot["maxFileSize"]),
        "snapshot.maxFileSize: invalid size"
    );
    let mut tasks = config.get("tasks").cloned().unwrap_or(json!({}));
    let key_pattern = Regex::new(r"^[A-Za-z0-9][A-Za-z0-9:._-]*$").expect("task name regex");
    for (name, task) in tasks.as_object_mut().context("tasks: expected map")? {
        ensure!(key_pattern.is_match(name), "invalid task name {name}");
        strict(
            task,
            &[
                "description",
                "command",
                "image",
                "env",
                "secretEnv",
                "workdir",
                "resources",
                "labels",
                "node",
                "timeout",
                "priority",
                "retry",
                "artifacts",
                "junit",
                "shards",
                "prefer",
            ],
            "task",
        )?;
        ensure!(
            task["command"]
                .as_str()
                .is_some_and(|command| !command.is_empty())
                || task["command"].as_array().is_some_and(
                    |command| !command.is_empty() && command.iter().all(Value::is_string)
                ),
            "task.command: expected command string or argv"
        );
        optional_strings(
            task,
            &[
                "description",
                "image",
                "workdir",
                "node",
                "timeout",
                "junit",
            ],
        )?;
        optional_arrays(task, &["secretEnv", "artifacts", "prefer"])?;
        if let Some(value) = task.get("resources") {
            validate_resources(value)?;
        }
        if let Some(value) = task.get("labels") {
            labels(value)?;
        }
        if let Some(value) = task.get("env") {
            string_record(value, "env", true)?;
        }
        if let Some(value) = task.get("priority") {
            ensure!(
                bounded_integer(value, -100, 100),
                "task.priority: out of range"
            );
        }
        if let Some(value) = task.get("shards") {
            ensure!(bounded_integer(value, 1, 64), "task.shards: out of range");
        }
        if let Some(retry) = task.get_mut("retry") {
            ensure!(retry.is_object(), "retry: expected object");
            let max = retry.get("max").cloned().unwrap_or(json!(0));
            let on = retry.get("on").cloned().unwrap_or(json!(["lost"]));
            ensure!(bounded_integer(&max, 0, 10), "retry.max: out of range");
            string_array(&on, "retry.on")?;
            ensure!(
                on.as_array()
                    .unwrap()
                    .iter()
                    .all(|value| value == "lost" || value == "failed"),
                "retry.on: invalid outcome"
            );
            *retry = json!({"max":max,"on":on});
        }
    }
    let builds = config.get("builds").cloned().unwrap_or(json!({}));
    for (name, build) in builds.as_object().context("builds: expected map")? {
        ensure!(key_pattern.is_match(name), "invalid build name {name}");
        strict(
            build,
            &[
                "description",
                "file",
                "context",
                "stage",
                "buildArgs",
                "tags",
                "platforms",
                "output",
                "secretEnv",
                "labels",
                "node",
            ],
            "build",
        )?;
        optional_strings(build, &["description", "file", "context", "stage", "node"])?;
        optional_arrays(build, &["tags", "platforms", "secretEnv"])?;
        if let Some(value) = build.get("buildArgs") {
            string_record(value, "buildArgs", false)?;
        }
        if let Some(value) = build.get("labels") {
            labels(value)?;
        }
        if let Some(value) = build.get("output") {
            ensure!(
                ["load", "push", "tar", "none"]
                    .iter()
                    .any(|output| value == output),
                "build.output: invalid output"
            );
        }
    }
    let actions = config.get("actions").cloned().unwrap_or(json!({}));
    strict(
        &actions,
        &["platforms", "dockerSocket", "secretEnv", "vars", "launcher"],
        "actions",
    )?;
    if let Some(value) = actions.get("dockerSocket") {
        ensure!(value.is_boolean(), "actions.dockerSocket: expected boolean");
    }
    optional_arrays(&actions, &["secretEnv"])?;
    for field in ["platforms", "vars"] {
        if let Some(value) = actions.get(field) {
            string_record(value, field, false)?;
        }
    }
    if let Some(launcher) = actions.get("launcher") {
        let workflows = launcher
            .as_object()
            .context("actions.launcher: expected workflow map")?;
        let file_pattern =
            Regex::new(r"^(?:\.github/workflows/)?[A-Za-z0-9_-][A-Za-z0-9_.-]*\.ya?ml$")
                .expect("launcher workflow filename regex");
        let mut seen = std::collections::BTreeSet::new();
        for (file, entry) in workflows {
            ensure!(
                file_pattern.is_match(file),
                "actions.launcher.{file}: expected workflow filename or .github/workflows path"
            );
            let name = file.strip_prefix(".github/workflows/").unwrap_or(file);
            ensure!(
                seen.insert(name),
                "actions.launcher.{file}: workflow is listed twice"
            );
            strict(entry, &["description", "jobs"], "actions.launcher entry")?;
            optional_strings(entry, &["description"])?;
            if let Some(jobs) = entry.get("jobs") {
                string_array(jobs, "actions.launcher.jobs")?;
                let jobs = jobs.as_array().unwrap();
                ensure!(
                    !jobs.is_empty()
                        && jobs
                            .iter()
                            .all(|job| !job.as_str().unwrap().trim().is_empty()),
                    "actions.launcher.{file}.jobs: expected nonempty job IDs"
                );
                let unique: std::collections::BTreeSet<_> =
                    jobs.iter().map(|job| job.as_str().unwrap()).collect();
                ensure!(
                    unique.len() == jobs.len(),
                    "actions.launcher.{file}.jobs: duplicate job IDs"
                );
            }
        }
    }
    config["version"] = json!(1);
    config["defaults"] = defaults;
    config["snapshot"] = snapshot;
    config["tasks"] = tasks;
    config["builds"] = builds;
    config["actions"] = actions;
    Ok(config)
}

pub fn discover(root: &Path) -> Result<Value> {
    let root = fs::canonicalize(root).context("locate discovery root")?;
    let config_path = ["gridstator.yml", "gridstator.yaml"]
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.exists());
    let raw = match config_path.as_ref() {
        Some(path) => {
            let value: Value = serde_yaml_ng::from_slice(&fs::read(path)?)
                .with_context(|| format!("Cannot parse {}", path.display()))?;
            if value.is_null() { json!({}) } else { value }
        }
        None => json!({}),
    };
    let config = normalize_project_config(raw).with_context(|| {
        config_path
            .as_ref()
            .and_then(|path| path.file_name())
            .map_or_else(
                || "Invalid project configuration".to_owned(),
                |name| format!("Invalid {}", name.to_string_lossy()),
            )
    })?;
    let mut result = json!({"root":root,"tasks":config["tasks"],"builds":config["builds"],"config":config,"workflows":discover_workflows(&root)?,"packages":discover_package_scripts(&root)?,"dockerfiles":discover_dockerfiles(&root)?});
    if let Some(path) = config_path {
        result["configPath"] = json!(path);
    }
    Ok(result)
}
