//! Wire defaults and validation kept separate from deserialization.
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};

/// States that retain a worker's resource reservation until a fenced transition releases it.
pub fn reserves_resources(state: &str) -> bool {
    matches!(state, "assigned" | "starting" | "running")
}

pub fn terminal(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "cancelled" | "lost")
}

pub fn can_transition(from: &str, to: &str) -> bool {
    match from {
        "created" => matches!(to, "queued" | "cancelled" | "failed"),
        "queued" => matches!(to, "assigned" | "cancelled" | "failed"),
        "assigned" => matches!(
            to,
            "queued" | "starting" | "running" | "succeeded" | "failed" | "cancelled" | "lost"
        ),
        "starting" => matches!(
            to,
            "running" | "succeeded" | "failed" | "cancelled" | "lost"
        ),
        "running" => terminal(to),
        _ => false,
    }
}

fn text(v: &Value, min: usize, max: usize, field: &str) -> Result<()> {
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{field}: expected string"))?;
    ensure!(
        (min..=max).contains(&s.chars().count()),
        "{field}: invalid length"
    );
    Ok(())
}

fn number(v: &Value, min: f64, max: f64, integer: bool, field: &str) -> Result<()> {
    let n = v
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("{field}: expected number"))?;
    ensure!(
        n.is_finite() && n >= min && n <= max && (!integer || n.fract() == 0.0),
        "{field}: out of range"
    );
    Ok(())
}

fn strings(v: &Value, min: usize, max: usize, len: usize, field: &str) -> Result<()> {
    let a = v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{field}: expected array"))?;
    ensure!(
        (min..=max).contains(&a.len()),
        "{field}: invalid array length"
    );
    for item in a {
        text(item, 0, len, field)?;
    }
    Ok(())
}

fn env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn env_map(v: &Value, field: &str) -> Result<()> {
    for (key, value) in v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{field}: expected object"))?
    {
        ensure!(env_name(key), "{field}: invalid variable name {key}");
        text(value, 0, 128 * 1024, field)?;
    }
    Ok(())
}

pub fn relative_path(path: &str, root_allowed: bool) -> bool {
    if root_allowed && path == "." {
        return true;
    }
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .as_bytes()
            .get(1)
            .is_some_and(|c| *c == b':' && path.as_bytes()[0].is_ascii_alphabetic())
        && path.split('/').all(|s| !matches!(s, "" | "." | ".."))
}

fn path(v: &Value, field: &str) -> Result<()> {
    text(v, 1, 1024, field)?;
    ensure!(
        relative_path(v.as_str().unwrap(), true),
        "{field}: expected normalized relative path"
    );
    Ok(())
}

fn label(s: &str) -> bool {
    let Ok(re) = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]*(=[A-Za-z0-9._:/-]+)?$") else {
        return false;
    };
    re.is_match(s)
}

// JSON numbers have JavaScript Number semantics at the wire boundary. Whole finite values
// within its safe range become integer values so downstream integer accessors retain them.
pub(crate) fn canonicalize_numbers(value: &mut Value) {
    match value {
        Value::Number(number) if number.is_f64() => {
            if let Some(number) = number.as_f64().filter(|number| {
                number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= 9_007_199_254_740_991.0
            }) {
                *value = json!(number as i64);
            }
        }
        Value::Array(values) => values.iter_mut().for_each(canonicalize_numbers),
        Value::Object(values) => values.values_mut().for_each(canonicalize_numbers),
        _ => {}
    }
}

pub fn normalize_spec(mut input: Value) -> Result<Value> {
    canonicalize_numbers(&mut input);
    let object = input
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("job: expected object"))?;
    let kind = object.get("kind").and_then(Value::as_str).unwrap_or("");
    let fields: &[&str] = match kind {
        "command" => &["image", "command", "artifacts"],
        "docker-build" => &[
            "context",
            "dockerfile",
            "stage",
            "buildArgs",
            "tags",
            "platforms",
            "noCache",
            "pull",
            "output",
            "cacheFrom",
            "cacheTo",
        ],
        "actions" => &[
            "workflow",
            "job",
            "event",
            "eventPayload",
            "inputs",
            "vars",
            "platforms",
            "matrix",
            "dockerSocket",
            "git",
        ],
        _ => bail!("kind: expected command, docker-build, or actions"),
    };
    let mut value = json!({"kind":kind});
    for key in [
        "name",
        "env",
        "secretEnv",
        "snapshot",
        "workdir",
        "resources",
        "target",
        "timeoutSeconds",
        "retry",
        "priority",
        "project",
    ]
    .iter()
    .chain(fields.iter())
    {
        if let Some(v) = object.get(*key) {
            value[*key] = v.clone();
        }
    }
    for (key, default) in [
        ("env", json!({})),
        ("secretEnv", json!({})),
        ("workdir", json!(".")),
        ("resources", json!({"cpus":2,"memoryMb":4096})),
        ("target", json!({"labels":[],"prefer":[]})),
        ("retry", json!({"max":0,"on":["lost"]})),
        ("priority", json!(0)),
    ] {
        if value.get(key).is_none() {
            value[key] = default;
        }
    }
    for (key, max) in [("name", 200), ("project", 200)] {
        if let Some(v) = value.get(key) {
            text(v, 1, max, key)?;
        }
    }
    for key in ["env", "secretEnv"] {
        env_map(&value[key], key)?;
    }
    text(&value["workdir"], 1, 1024, "workdir")?;
    let workdir = value["workdir"].as_str().unwrap();
    ensure!(
        workdir == "."
            || (!workdir.starts_with('/')
                && !workdir.contains('\\')
                && workdir.split('/').all(|s| !matches!(s, "" | "." | ".."))),
        "workdir: expected normalized relative path"
    );
    ensure!(value["resources"].is_object(), "resources: expected object");
    number(
        &value["resources"]["cpus"],
        f64::from_bits(1),
        1024.0,
        false,
        "resources.cpus",
    )?;
    number(
        &value["resources"]["memoryMb"],
        16.0,
        (16 * 1024 * 1024) as f64,
        true,
        "resources.memoryMb",
    )?;
    value["resources"] =
        json!({"cpus":value["resources"]["cpus"],"memoryMb":value["resources"]["memoryMb"]});
    ensure!(value["target"].is_object(), "target: expected object");
    let target = value["target"].clone();
    value["target"] = json!({"labels":target.get("labels").cloned().unwrap_or(json!([])),"prefer":target.get("prefer").cloned().unwrap_or(json!([]))});
    if let Some(node) = target.get("node") {
        text(node, 1, 128, "target.node")?;
        value["target"]["node"] = node.clone();
    }
    strings(&value["target"]["labels"], 0, 32, 128, "target.labels")?;
    for s in value["target"]["labels"].as_array().unwrap() {
        ensure!(label(s.as_str().unwrap()), "target.labels: invalid label");
    }
    strings(&value["target"]["prefer"], 0, 16, 128, "target.prefer")?;
    for s in value["target"]["prefer"].as_array().unwrap() {
        text(s, 1, 128, "target.prefer")?;
    }
    if let Some(v) = value.get("timeoutSeconds") {
        number(v, 1.0, 604800.0, true, "timeoutSeconds")?;
    }
    number(&value["priority"], -100.0, 100.0, true, "priority")?;
    ensure!(value["retry"].is_object(), "retry: expected object");
    let retry = value["retry"].clone();
    value["retry"] = json!({"max":retry.get("max").cloned().unwrap_or(json!(0)),"on":retry.get("on").cloned().unwrap_or(json!(["lost"]))});
    number(&value["retry"]["max"], 0.0, 10.0, true, "retry.max")?;
    strings(&value["retry"]["on"], 0, usize::MAX, 6, "retry.on")?;
    for outcome in value["retry"]["on"].as_array().unwrap() {
        ensure!(
            matches!(outcome.as_str(), Some("lost" | "failed")),
            "retry.on: invalid outcome"
        );
    }
    if let Some(snapshot) = value.get("snapshot") {
        ensure!(
            valid_digest(snapshot.as_str().unwrap_or("")),
            "snapshot: invalid digest"
        );
    }
    match kind {
        "command" => {
            if value.get("image").is_none() {
                value["image"] = json!("node:24-bookworm");
            }
            if value.get("artifacts").is_none() {
                value["artifacts"] = json!([]);
            }
            text(&value["image"], 1, 512, "image")?;
            strings(&value["command"], 1, 4096, 256 * 1024, "command")?;
            strings(&value["artifacts"], 0, 32, 1024, "artifacts")?;
            for p in value["artifacts"].as_array().unwrap() {
                path(p, "artifacts")?;
            }
        }
        "docker-build" => {
            for (key, default) in [
                ("context", json!(".")),
                ("buildArgs", json!({})),
                ("tags", json!([])),
                ("platforms", json!([])),
                ("noCache", json!(false)),
                ("pull", json!(false)),
                ("output", json!("load")),
                ("cacheFrom", json!([])),
                ("cacheTo", json!([])),
            ] {
                if value.get(key).is_none() {
                    value[key] = default;
                }
            }
            path(&value["context"], "context")?;
            if let Some(v) = value.get("dockerfile") {
                path(v, "dockerfile")?;
            }
            if let Some(v) = value.get("stage") {
                text(v, 1, 128, "stage")?;
            }
            for key in ["tags", "platforms", "cacheFrom", "cacheTo"] {
                strings(
                    &value[key],
                    0,
                    if key == "tags" { 32 } else { 8 },
                    if key == "platforms" { 64 } else { 512 },
                    key,
                )?;
                for item in value[key].as_array().unwrap() {
                    text(item, 1, if key == "platforms" { 64 } else { 512 }, key)?;
                }
            }
            for key in ["noCache", "pull"] {
                ensure!(value[key].is_boolean(), "{key}: expected boolean");
            }
            ensure!(
                matches!(
                    value["output"].as_str(),
                    Some("load" | "push" | "tar" | "none")
                ),
                "output: invalid mode"
            );
            let build_args = value["buildArgs"]
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("buildArgs: expected object"))?;
            let re = regex::Regex::new(r"^[A-Za-z_][A-Za-z0-9_.-]*$")?;
            for (name, v) in build_args {
                ensure!(re.is_match(name), "buildArgs: invalid name");
                text(v, 0, 64 * 1024, "buildArgs")?;
            }
        }
        "actions" => {
            for (key, default) in [
                ("event", json!("push")),
                ("inputs", json!({})),
                ("vars", json!({})),
                ("platforms", json!({})),
                ("matrix", json!([])),
                ("dockerSocket", json!(false)),
            ] {
                if value.get(key).is_none() {
                    value[key] = default;
                }
            }
            path(&value["workflow"], "workflow")?;
            if let Some(v) = value.get("job") {
                text(v, 1, 200, "job")?;
            }
            let event = value["event"].as_str().unwrap_or("");
            ensure!(
                !event.is_empty() && event.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "event: invalid event"
            );
            env_map(&value["vars"], "vars")?;
            for key in ["inputs", "platforms"] {
                let object = value[key]
                    .as_object()
                    .ok_or_else(|| anyhow::anyhow!("{key}: expected object"))?;
                for (k, v) in object {
                    ensure!(
                        (1..=if key == "inputs" { 200 } else { 100 }).contains(&k.chars().count()),
                        "{key}: invalid key length"
                    );
                    text(
                        v,
                        if key == "inputs" { 0 } else { 1 },
                        if key == "inputs" { 64 * 1024 } else { 512 },
                        key,
                    )?;
                }
            }
            strings(&value["matrix"], 0, 32, 200, "matrix")?;
            for matrix in value["matrix"].as_array().unwrap() {
                ensure!(
                    matrix
                        .as_str()
                        .unwrap()
                        .split_once(':')
                        .is_some_and(|(a, b)| !a.is_empty()
                            && !b.is_empty()
                            && !b.contains(['\n', '\r', '\u{2028}', '\u{2029}'])),
                    "matrix: invalid combination"
                );
            }
            ensure!(
                value["dockerSocket"].is_boolean(),
                "dockerSocket: expected boolean"
            );
            if let Some(v) = value.get("eventPayload") {
                ensure!(v.is_object(), "eventPayload: expected object");
            }
            if let Some(v) = value.get("git") {
                ensure!(v.is_object(), "git: expected object");
                for (key, max) in [("branch", 255), ("remoteUrl", 1024), ("headSha", 64)] {
                    if let Some(v) = v.get(key) {
                        text(v, 0, max, key)?;
                    }
                }
                let mut git = json!({});
                for key in ["branch", "remoteUrl", "headSha"] {
                    if let Some(v) = v.get(key) {
                        git[key] = v.clone();
                    }
                }
                value["git"] = git;
            }
        }
        _ => unreachable!(),
    }
    Ok(value)
}

pub fn valid_digest(s: &str) -> bool {
    s.strip_prefix("sha256:").is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

pub fn redact_spec(spec: &Value) -> Value {
    let mut out = spec.clone();
    if let Some(secrets) = out.get_mut("secretEnv").and_then(Value::as_object_mut) {
        for value in secrets.values_mut() {
            *value = json!("***");
        }
    }
    out
}

pub fn clip_text(value: &str, max_utf16: usize) -> String {
    let mut units = 0;
    value
        .chars()
        .take_while(|c| {
            units += c.len_utf16();
            units <= max_utf16
        })
        .collect()
}

pub fn normalize_timestamp(mut input: Value) -> Result<Value> {
    canonicalize_numbers(&mut input);
    number(&input, 0.0, 9_007_199_254_740_991.0, true, "timestamp")?;
    Ok(input)
}
pub fn normalize_outputs(mut input: Value) -> Result<Value> {
    canonicalize_numbers(&mut input);
    ensure!(input.is_object(), "outputs: expected object");
    let mut out = json!({});
    if let Some(steps) = input.get("steps") {
        let steps = steps
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("steps: expected array"))?;
        ensure!(steps.len() <= 5000, "steps: too many entries");
        let mut normalized = vec![];
        for step in steps {
            ensure!(step.is_object(), "step: expected object");
            for (key, max) in [("id", 300), ("name", 1000)] {
                text(&step[key], 0, max, key)?;
            }
            ensure!(step.get("group").is_some(), "step.group: required");
            if !step["group"].is_null() {
                text(&step["group"], 0, 300, "step.group")?;
            }
            ensure!(
                matches!(
                    step["status"].as_str(),
                    Some("running" | "success" | "failure" | "skipped" | "cancelled")
                ),
                "step.status: invalid status"
            );
            ensure!(
                step.get("startedAt").is_some() && step.get("durationMs").is_some(),
                "step timing fields required"
            );
            if !step["startedAt"].is_null() {
                normalize_timestamp(step["startedAt"].clone())?;
            }
            let duration = if step["durationMs"].is_null() {
                Value::Null
            } else {
                let value = step["durationMs"]
                    .as_f64()
                    .filter(|n| n.is_finite())
                    .ok_or_else(|| anyhow::anyhow!("step.durationMs: expected finite number"))?
                    .max(0.0);
                if value.fract() == 0.0 && value < i64::MAX as f64 {
                    json!(value as i64)
                } else {
                    json!(value)
                }
            };
            normalized.push(json!({"id":step["id"],"group":step["group"],"name":step["name"],"status":step["status"],"startedAt":step["startedAt"],"durationMs":duration}));
        }
        out["steps"] = json!(normalized);
    }
    if let Some(image) = input.get("image") {
        ensure!(image.is_object(), "image: expected object");
        for key in ["id", "digest"] {
            ensure!(image.get(key).is_some(), "image.{key}: required");
            if !image[key].is_null() {
                text(&image[key], 0, usize::MAX, key)?;
            }
        }
        strings(&image["tags"], 0, usize::MAX, usize::MAX, "image.tags")?;
        ensure!(
            image.get("sizeBytes").is_some(),
            "image.sizeBytes: required"
        );
        if !image["sizeBytes"].is_null() {
            number(
                &image["sizeBytes"],
                0.0,
                9_007_199_254_740_991.0,
                true,
                "image.sizeBytes",
            )?;
        }
        ensure!(
            matches!(
                image["output"].as_str(),
                Some("load" | "push" | "tar" | "none")
            ),
            "image.output: invalid mode"
        );
        out["image"] = json!({"id":image["id"],"digest":image["digest"],"tags":image["tags"],"sizeBytes":image["sizeBytes"],"output":image["output"]});
    }
    Ok(out)
}

pub fn normalize_result(mut input: Value) -> Result<Value> {
    canonicalize_numbers(&mut input);
    ensure!(input.is_object(), "result: expected object");
    for key in ["jobId", "assignmentId"] {
        text(&input[key], 0, usize::MAX, key)?;
    }
    ensure!(
        matches!(
            input["outcome"].as_str(),
            Some("succeeded" | "failed" | "cancelled" | "lost")
        ),
        "result.outcome: invalid outcome"
    );
    for key in ["exitCode", "error"] {
        ensure!(input.get(key).is_some(), "result.{key}: required");
    }
    if !input["exitCode"].is_null() {
        number(
            &input["exitCode"],
            -9_007_199_254_740_991.0,
            9_007_199_254_740_991.0,
            true,
            "result.exitCode",
        )?;
    }
    if !input["error"].is_null() {
        text(&input["error"], 0, 8192, "result.error")?;
    }
    for key in ["finishedAt", "logSeq"] {
        number(&input[key], 0.0, 9_007_199_254_740_991.0, true, key)?;
    }
    let oom = input.get("oomKilled").cloned().unwrap_or(json!(false));
    ensure!(oom.is_boolean(), "oomKilled: expected boolean");
    let outputs = match input.get("outputs") {
        None | Some(Value::Null) => Value::Null,
        Some(value) => normalize_outputs(value.clone())?,
    };
    let artifacts = input.get("artifacts").cloned().unwrap_or(json!([]));
    let artifacts = artifacts
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("artifacts: expected array"))?;
    ensure!(artifacts.len() <= 1000, "artifacts: too many entries");
    let mut normalized = vec![];
    for artifact in artifacts {
        text(&artifact["name"], 1, 1024, "artifact.name")?;
        ensure!(
            valid_digest(artifact["digest"].as_str().unwrap_or("")),
            "artifact.digest: invalid digest"
        );
        number(
            &artifact["size"],
            0.0,
            9_007_199_254_740_991.0,
            true,
            "artifact.size",
        )?;
        normalized.push(
            json!({"name":artifact["name"],"digest":artifact["digest"],"size":artifact["size"]}),
        );
    }
    Ok(
        json!({"jobId":input["jobId"],"assignmentId":input["assignmentId"],"outcome":input["outcome"],"exitCode":input["exitCode"],"error":input["error"],"oomKilled":oom,"finishedAt":input["finishedAt"],"logSeq":input["logSeq"],"outputs":outputs,"artifacts":normalized}),
    )
}
