use gridstator::scheduler::{ScheduleContext, schedule};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::slice::from_ref;

fn node(id: &str) -> Value {
    json!({"id":id,"name":id,"online":true,
        "info":{"cpus":16,"memoryTotalMb":65536,"executors":["command"],"docker":{"available":true}},
        "metrics":null,"settings":{"maxConcurrency":4,"reserved":{"cpus":0,"memoryMb":0},"schedulingMode":"enabled"},
        "effectiveLabels":["os=linux","arch=x64","docker"],"allocated":{"cpus":0,"memoryMb":0,"jobs":0}})
}

fn job(id: &str, queued: u64) -> Value {
    json!({"id":id,"kind":"command","priority":0,"queuedAt":queued,
        "resources":{"cpus":2,"memoryMb":4096},"target":{"labels":[],"prefer":[]}})
}

fn placements(jobs: &[Value], nodes: &[Value]) -> Vec<(String, String)> {
    schedule(jobs, nodes, &ScheduleContext::default())
        .assignments
        .into_iter()
        .map(|assignment| (assignment.job_id, assignment.node_id))
        .collect()
}

#[test]
fn priority_fifo_backfill_and_same_pass_reservations() {
    let mut worker = node("worker");
    worker["info"]["cpus"] = json!(4);
    let low = job("low", 1);
    let mut high = job("high", 2);
    high["priority"] = json!(5);
    let mut huge = job("huge", 0);
    huge["priority"] = json!(10);
    huge["resources"]["cpus"] = json!(8);
    let tail = job("tail", 3);
    let jobs = vec![low, tail, high, huge];
    let nodes = vec![worker];
    let original = nodes.clone();
    let result = schedule(&jobs, &nodes, &ScheduleContext::default());
    assert_eq!(
        result
            .assignments
            .iter()
            .map(|a| a.job_id.as_str())
            .collect::<Vec<_>>(),
        ["high", "low"]
    );
    assert!(result.pending["huge"].contains("allows at most 4 CPU"));
    assert!(result.pending["tail"].contains("waiting for 2 CPU"));
    assert_eq!(
        nodes, original,
        "the scheduling pass must not mutate caller allocations"
    );
}

#[test]
fn reservations_fractional_cpus_and_live_memory_protect_host() {
    let mut laptop = node("laptop");
    laptop["info"]["cpus"] = json!(8);
    laptop["settings"]["reserved"] = json!({"cpus":6.5,"memoryMb":57344});
    let mut small = job("small", 0);
    small["resources"] = json!({"cpus":0.5,"memoryMb":1024});
    let large = job("large", 1);
    let result = schedule(
        &[small.clone(), large],
        &[laptop.clone()],
        &ScheduleContext::default(),
    );
    assert_eq!(result.assignments.len(), 1);
    assert!(result.pending["large"].contains("allows at most 1.5 CPU, 8 GiB"));
    laptop["metrics"] = json!({"cpuPercent":10,"memoryAvailableMb":512});
    let result = schedule(&[small], &[laptop], &ScheduleContext::default());
    assert!(result.assignments.is_empty());
    assert!(result.pending["small"].contains("memory available right now"));
}

#[test]
fn concurrency_and_scheduling_modes_are_hard_constraints() {
    let mut limited = node("limited");
    limited["settings"]["maxConcurrency"] = json!(1);
    let result = schedule(
        &[job("a", 0), job("b", 1)],
        &[limited.clone()],
        &ScheduleContext::default(),
    );
    assert_eq!(result.assignments.len(), 1);
    assert!(result.pending["b"].contains("concurrency limit (1)"));
    for mode in ["disabled", "draining"] {
        limited["settings"]["schedulingMode"] = json!(mode);
        let result = schedule(
            &[job("a", 0)],
            &[limited.clone()],
            &ScheduleContext::default(),
        );
        assert_eq!(result.pending["a"], format!("node limited is {mode}"));
    }
}

#[test]
fn capabilities_labels_and_explicit_name_or_id_are_enforced() {
    let a = node("plain");
    let mut b = node("gpu-id");
    b["name"] = json!("gpu");
    b["effectiveLabels"] = json!(["docker", "gpu"]);
    let mut request = job("work", 0);
    request["target"]["labels"] = json!(["gpu"]);
    assert_eq!(
        placements(from_ref(&request), &[a.clone(), b.clone()]),
        [("work".into(), "gpu-id".into())]
    );
    for target in ["gpu", "gpu-id"] {
        request["target"]["node"] = json!(target);
        assert_eq!(
            placements(from_ref(&request), &[a.clone(), b.clone()])[0].1,
            "gpu-id"
        );
    }
    request["target"]["node"] = json!("missing");
    assert_eq!(
        schedule(&[request], &[a], &ScheduleContext::default()).pending["work"],
        "node \"missing\" does not exist"
    );
    b["info"]["executors"] = json!([]);
    b["info"]["docker"]["available"] = json!(false);
    assert!(
        schedule(&[job("work", 0)], &[b], &ScheduleContext::default()).pending["work"]
            .contains("Docker unavailable")
    );
}

#[test]
fn pending_reasons_explain_closest_candidate() {
    let request = job("work", 0);
    assert_eq!(
        schedule(from_ref(&request), &[], &ScheduleContext::default()).pending["work"],
        "no worker nodes are registered"
    );
    let mut a = node("a");
    a["online"] = json!(false);
    assert_eq!(
        schedule(
            from_ref(&request),
            &[a.clone()],
            &ScheduleContext::default()
        )
        .pending["work"],
        "no worker nodes are online"
    );
    let mut labeled = request;
    labeled["target"]["labels"] = json!(["tpu"]);
    assert_eq!(
        schedule(
            &[labeled],
            &[a, node("b"), node("c")],
            &ScheduleContext::default()
        )
        .pending["work"],
        "no eligible node has label(s) tpu"
    );
}

#[test]
fn preferences_and_affinity_are_soft_with_deterministic_ties() {
    let a = node("a");
    let b = node("b");
    let mut request = job("work", 0);
    request["project"] = json!("web");
    let context = ScheduleContext {
        warm_nodes: HashMap::from([("web".into(), HashSet::from(["b".into()]))]),
    };
    assert_eq!(
        schedule(from_ref(&request), &[b.clone(), a.clone()], &context).assignments[0].node_id,
        "b"
    );
    assert_eq!(
        placements(from_ref(&request), &[b.clone(), a.clone()])[0].1,
        "a"
    );
    request["target"]["prefer"] = json!(["b"]);
    assert_eq!(
        placements(from_ref(&request), &[a.clone(), b.clone()])[0].1,
        "b"
    );
    let mut full = b;
    full["allocated"]["jobs"] = json!(4);
    assert_eq!(placements(&[request], &[a, full])[0].1, "a");
}

#[test]
fn shards_spread_count_running_siblings_and_fall_back_to_one_worker() {
    let jobs: Vec<_> = (0..4)
        .map(|i| {
            let mut request = job(&format!("shard-{i}"), i);
            request["group"] = json!("group");
            request
        })
        .collect();
    let result = schedule(&jobs, &[node("a"), node("b")], &ScheduleContext::default());
    assert_eq!(
        result
            .assignments
            .iter()
            .filter(|a| a.node_id == "a")
            .count(),
        2
    );
    assert_eq!(
        result
            .assignments
            .iter()
            .filter(|a| a.node_id == "b")
            .count(),
        2
    );
    assert_eq!(placements(&jobs, &[node("a")]).len(), 4);
    let mut a = node("a");
    a["groups"] = json!({"group":1});
    assert_eq!(placements(&jobs[..1], &[a, node("b")])[0].1, "b");
}

#[test]
fn rejected_workers_are_excluded_and_public_records_are_supported() {
    let mut request = job("work", 0);
    request["excludedNodes"] = json!(["a"]);
    assert_eq!(
        placements(from_ref(&request), &[node("a"), node("b")])[0].1,
        "b"
    );
    let api_job = json!({"id":"public","queuedAt":0,"spec":{"kind":"command","resources":{"cpus":1,"memoryMb":1024},"target":{"labels":[],"prefer":[]}}});
    let mut api_node = node("worker");
    api_node.as_object_mut().unwrap().remove("online");
    api_node["status"] = json!("online");
    api_node["allocated"]
        .as_object_mut()
        .unwrap()
        .remove("jobs");
    api_node["activeJobs"] = json!(["one", "two", "three", "four"]);
    assert!(placements(from_ref(&api_job), from_ref(&api_node)).is_empty());
    api_node["activeJobs"] = json!([]);
    assert_eq!(
        placements(&[api_job], &[api_node]),
        [("public".into(), "worker".into())]
    );
}

#[test]
fn docker_execution_capacity_bounds_host_capacity_without_changing_host_info() {
    let mut mac = node("mac");
    mac["info"]["executionCapacity"] = json!({"cpus":4,"memoryTotalMb":8192});
    mac["settings"]["reserved"] = json!({"cpus":1,"memoryMb":2048});
    assert_eq!(gridstator::scheduler::allocatable(&mac), (3.0, 6144.0));
    assert_eq!(mac["info"]["cpus"], 16);
    let mut too_large = job("large", 0);
    too_large["resources"] = json!({"cpus":4,"memoryMb":4096});
    assert!(
        schedule(&[too_large], &[mac], &ScheduleContext::default())
            .assignments
            .is_empty()
    );
}
