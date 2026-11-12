//! Behavior fixtures for the native implementation.
use gridstator::{protocol, workspace};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("contracts/behavior")
        .join(name);
    serde_json::from_slice(&std::fs::read(path).expect("read behavior fixture"))
        .expect("valid behavior JSON")
}

#[test]
fn manifest_canonical_bytes_and_ids_match_expected_behavior() {
    for case in fixture("manifests.json")["cases"]
        .as_array()
        .expect("valid manifests")
    {
        let input = serde_json::to_vec(&case["input"]).expect("manifest JSON");
        let parsed = workspace::parse_manifest(&input).expect("valid manifest");
        let bytes = workspace::serialize_manifest(&parsed).expect("canonical manifest");
        assert_eq!(
            bytes,
            case["canonical"]
                .as_str()
                .expect("canonical string")
                .as_bytes(),
            "canonical bytes: {}",
            case["id"]
        );
        assert_eq!(
            hex::encode(&bytes),
            case["canonicalUtf8Hex"].as_str().expect("canonical hex"),
            "UTF-8 encoding: {}",
            case["id"]
        );
        assert_eq!(
            workspace::manifest_id(&parsed).expect("manifest ID"),
            case["sha256"].as_str().expect("manifest hash"),
            "snapshot ID: {}",
            case["id"]
        );
    }
}

#[test]
fn manifest_structural_rejections_match_expected_behavior() {
    for case in fixture("manifests.json")["invalid"]
        .as_array()
        .expect("invalid manifests")
    {
        let input = serde_json::to_vec(&case["input"]).expect("manifest JSON");
        assert!(
            workspace::parse_manifest(&input).is_err(),
            "accepted invalid manifest: {}",
            case["id"]
        );
    }
}

#[test]
fn complete_job_transition_matrix_and_terminal_states_match_expected_behavior() {
    let reference = fixture("job-transitions.json");
    let cases = reference["cases"].as_array().expect("transitions");
    assert_eq!(cases.len(), 81);
    for case in cases {
        let from = case["from"].as_str().expect("from state");
        let to = case["to"].as_str().expect("to state");
        assert_eq!(
            protocol::can_transition(from, to),
            case["allowed"].as_bool().expect("transition result"),
            "{from} -> {to}"
        );
    }
    let terminal = reference["terminal"].as_array().expect("terminal states");
    let dispatched = reference["dispatched"]
        .as_array()
        .expect("reservation states");
    for state in reference["states"].as_array().expect("states") {
        assert_eq!(
            protocol::reserves_resources(state.as_str().unwrap()),
            dispatched.contains(state),
            "reservation membership: {state}"
        );
    }
    assert!(!protocol::reserves_resources("unknown"));
    for state in reference["states"].as_array().expect("states") {
        assert_eq!(
            protocol::terminal(state.as_str().expect("state name")),
            terminal.contains(state),
            "terminal: {state}"
        );
    }
}

#[test]
fn job_spec_defaults_validation_and_unknown_fields_match_expected_behavior() {
    for case in fixture("schema-cases.json")["cases"]
        .as_array()
        .expect("schema cases")
        .iter()
        .filter(|case| case["schema"] == "JobSpec")
    {
        let result = protocol::normalize_spec(case["input"].clone());
        if case["success"] == true {
            let output = result
                .unwrap_or_else(|error| panic!("rejected valid spec {}: {error:#}", case["id"]));
            assert_eq!(output, case["output"], "defaults/output: {}", case["id"]);
        } else {
            assert!(result.is_err(), "accepted invalid spec: {}", case["id"]);
        }
    }
}

#[test]
fn timestamp_and_step_telemetry_match_expected_behavior() {
    for case in fixture("schema-cases.json")["cases"]
        .as_array()
        .expect("schema cases")
    {
        let result = match case["schema"].as_str() {
            Some("Timestamp") => protocol::normalize_timestamp(case["input"].clone()),
            Some("JobStep") => {
                protocol::normalize_outputs(serde_json::json!({"steps":[case["input"]]}))
                    .map(|outputs| outputs["steps"][0].clone())
            }
            _ => continue,
        };
        if case["success"] == true {
            assert_eq!(
                result.unwrap_or_else(|error| panic!("{}: {error:#}", case["id"])),
                case["output"],
                "{}",
                case["id"]
            );
        } else {
            assert!(result.is_err(), "accepted invalid {}", case["id"]);
        }
    }
}
