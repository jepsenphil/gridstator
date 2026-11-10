use gridstator::protocol::{clip_text, normalize_outputs, normalize_result, normalize_spec};
use serde_json::json;

#[test]
fn build_and_actions_collection_bounds_match_the_protocol() {
    for input in [
        json!({"kind":"docker-build","tags":[""]}),
        json!({"kind":"docker-build","platforms":["a".repeat(65)]}),
        json!({"kind":"docker-build","cacheFrom":vec!["cache";9]}),
        json!({"kind":"actions","workflow":"ci.yml","platforms":{"ubuntu-latest":""}}),
        json!({"kind":"actions","workflow":"ci.yml","inputs":{"a".repeat(201):"x"}}),
    ] {
        assert!(normalize_spec(input).is_err());
    }
    let normalized = normalize_spec(
        json!({"kind":"actions","workflow":"ci.yml","git":{"branch":"main","unknown":true}}),
    )
    .unwrap();
    assert_eq!(normalized["git"], json!({"branch":"main"}));
}

#[test]
fn negative_step_telemetry_is_normalized_without_rejecting_a_valid_result() {
    let outputs = json!({"steps":[{"id":"step","group":null,"name":"build","status":"success","startedAt":1000,"durationMs":-10}]});
    let parsed = normalize_outputs(outputs.clone()).unwrap();
    assert_eq!(parsed["steps"][0]["durationMs"], 0);
    let mut result = json!({"jobId":"job","assignmentId":"assignment","outcome":"succeeded","exitCode":0,"error":null,"finishedAt":1000,"logSeq":1,"outputs":outputs});
    let parsed = normalize_result(result.clone()).unwrap();
    assert_eq!(parsed["outputs"]["steps"][0]["durationMs"], 0);
    assert_eq!(parsed["oomKilled"], false);
    assert_eq!(parsed["artifacts"], json!([]));
    result["logSeq"] = json!(-1);
    assert!(normalize_result(result).is_err());
}

#[test]
fn error_clipping_preserves_utf16_limit_and_utf8_boundaries() {
    let value = "🦀".repeat(5000);
    let clipped = clip_text(&value, 8192);
    assert_eq!(clipped.encode_utf16().count(), 8192);
    assert!(clipped.ends_with('🦀'));
}
