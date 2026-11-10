use gridstator::scheduler::{ScheduleContext, schedule};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

#[test]
fn frozen_source_scheduler_scenarios_preserve_placements_scores_and_pending_reasons() {
    let fixture: Value =
        serde_json::from_str(include_str!("../contracts/scheduler-v1.json")).unwrap();
    let mut covered = HashSet::new();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["id"].as_str().unwrap();
        let jobs = case["jobs"].as_array().unwrap();
        let nodes = case["nodes"].as_array().unwrap();
        let before = (jobs.clone(), nodes.clone());
        let warm_nodes: HashMap<String, HashSet<String>> = case["warmNodes"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(project, nodes)| {
                (
                    project.clone(),
                    nodes
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|n| n.as_str().unwrap().to_owned())
                        .collect(),
                )
            })
            .collect();
        let actual =
            serde_json::to_value(schedule(jobs, nodes, &ScheduleContext { warm_nodes })).unwrap();
        assert_eq!(
            actual["pending"], case["expected"]["pending"],
            "{name}: pending reasons"
        );
        let expected = case["expected"]["assignments"].as_array().unwrap();
        let assignments = actual["assignments"].as_array().unwrap();
        assert_eq!(
            assignments.len(),
            expected.len(),
            "{name}: assignment count"
        );
        for (actual, expected) in assignments.iter().zip(expected) {
            assert_eq!(actual["jobId"], expected["jobId"], "{name}: job/order");
            assert_eq!(actual["nodeId"], expected["nodeId"], "{name}: placement");
            assert!(
                (actual["score"].as_f64().unwrap() - expected["score"].as_f64().unwrap()).abs()
                    < 1e-12,
                "{name}: score {actual} != {expected}"
            );
        }
        assert_eq!(
            (jobs.clone(), nodes.clone()),
            before,
            "{name}: inputs changed"
        );
        covered.extend(
            case["ledgerRows"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap().to_owned()),
        );
    }
    for index in 1..=16 {
        assert!(covered.contains(&format!("reference-scheduler-index-{index:03}")));
    }
}
