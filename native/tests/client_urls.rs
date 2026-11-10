use gridstator::client::{ControllerError, normalize_url};
use serde_json::Value;

#[test]
fn controller_origins_match_the_frozen_source_url_normalizer() {
    let fixture: Value =
        serde_json::from_str(include_str!("../contracts/client-urls-v1.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let result = normalize_url(case["input"].as_str().unwrap());
        if let Some(expected) = case.get("value") {
            assert_eq!(
                result.unwrap(),
                expected.as_str().unwrap(),
                "{}",
                case["id"]
            );
        } else {
            let error = result.unwrap_err();
            let error = error.downcast_ref::<ControllerError>().unwrap();
            assert_eq!(
                error.code.as_deref(),
                case["error"]["code"].as_str(),
                "{}",
                case["id"]
            );
            assert_eq!(
                error.message,
                case["error"]["message"].as_str().unwrap(),
                "{}",
                case["id"]
            );
            assert_eq!(
                error.hint.as_deref(),
                case["error"]["hint"].as_str(),
                "{}",
                case["id"]
            );
        }
    }
}
