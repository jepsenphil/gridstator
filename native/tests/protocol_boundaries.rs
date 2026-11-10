//! Differential minimum/maximum/null/omission/UTF-16 cases from the immutable v1 schemas.
use gridstator::protocol;
use serde_json::{Map, Value};
fn expand(input: &Value) -> Value {
    if let Some(object) = input.as_object() {
        if object.len() == 1 {
            if let Some(text) = object.get("$text") {
                return Value::String(
                    text[0]
                        .as_str()
                        .unwrap()
                        .repeat(text[1].as_u64().unwrap() as usize),
                );
            }
            if let Some(array) = object.get("$array") {
                return Value::Array(vec![expand(&array[0]); array[1].as_u64().unwrap() as usize]);
            }
        }
        return Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), expand(value)))
                .collect::<Map<_, _>>(),
        );
    }
    match input {
        Value::Array(values) => Value::Array(values.iter().map(expand).collect()),
        _ => input.clone(),
    }
}
#[test]
fn every_source_boundary_matches_native_acceptance_and_normalization() {
    let fixture: Value =
        serde_json::from_str(include_str!("../contracts/protocol-boundaries-v1.json")).unwrap();
    let mut failures = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let input = expand(&case["input"]);
        let result = match case["schema"].as_str().unwrap() {
            "JobSpec" => protocol::normalize_spec(input),
            "Timestamp" => protocol::normalize_timestamp(input),
            "JobOutputs" => protocol::normalize_outputs(input),
            "AgentJobResult" => protocol::normalize_result(input),
            "ProjectConfig" => gridstator::discovery::normalize_project_config(input),
            _ => panic!("Unknown fixture schema"),
        };
        let success = case["success"] == true;
        if result.is_ok() != success {
            failures.push(format!(
                "{} acceptance: expected {success}, error {:?}",
                case["id"],
                result.as_ref().err()
            ));
            continue;
        }
        if let Ok(actual) = result {
            let expected = expand(&case["output"]);
            if actual != expected {
                failures.push(format!("{} normalized output differs", case["id"]));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} differential failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn whole_json_numbers_survive_downstream_integer_accessors() {
    let input: Value=serde_json::from_str(r#"{"kind":"command","command":["true"],"resources":{"cpus":1.0,"memoryMb":16.0},"priority":-2.0,"retry":{"max":2.0},"timeoutSeconds":1.0}"#).unwrap();
    let spec = protocol::normalize_spec(input).unwrap();
    assert_eq!(spec["resources"]["memoryMb"].as_u64(), Some(16));
    assert_eq!(spec["priority"].as_i64(), Some(-2));
    assert_eq!(spec["retry"]["max"].as_u64(), Some(2));
    assert_eq!(spec["timeoutSeconds"].as_u64(), Some(1));
}

#[tokio::test]
async fn source_accepted_nul_paths_are_rejected_before_filesystem_or_tool_use() {
    use gridstator::{
        executor::{self, ExecutionContext},
        workspace,
    };
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("source");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
    std::fs::write(root.join("workflow.yml"), "jobs: {}\n").unwrap();
    let sentinel = temporary.path().join("outside");
    std::fs::write(&sentinel, "unchanged").unwrap();
    let capture = workspace::capture(&root, workspace::CaptureOptions::default()).unwrap();
    let store = workspace::BlobStore::new(temporary.path().join("cas"));
    store.init().unwrap();
    for (digest, source) in &capture.blobs {
        store
            .put_bytes(digest, &source.read_verified(digest).unwrap())
            .unwrap();
    }
    for spec in [
        serde_json::json!({"kind":"docker-build","dockerfile":"Dockerfile\u{0}suffix"}),
        serde_json::json!({"kind":"actions","workflow":"workflow.yml\u{0}suffix"}),
    ] {
        let spec = protocol::normalize_spec(spec).expect("source accepts NUL at schema level");
        let id = ulid::Ulid::new().to_string();
        let context = ExecutionContext {
            job_id: id.clone(),
            assignment_id: ulid::Ulid::new().to_string(),
            attempt: 1,
            node_id: "nul-guard-contract".into(),
            node_name: "nul-guard-contract".into(),
            spec,
            work_dir: temporary.path().join(&id),
            manifest: Some(capture.manifest.clone()),
            blobs: store.clone(),
            keep_failed: false,
            act_bin: None,
        };
        let (logs, _receive) = tokio::sync::mpsc::channel(32);
        let result = executor::execute(context, tokio_util::sync::CancellationToken::new(), logs)
            .await
            .unwrap();
        assert_eq!(result.outcome, "failed");
        assert!(
            result
                .error
                .as_deref()
                .unwrap()
                .contains("path contains NUL"),
            "{:?}",
            result.error
        );
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "unchanged");
    }
}
