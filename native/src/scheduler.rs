//! Pure placement policy. Inputs may be scheduler records or public job/node JSON records.
//!
//! Validation belongs at the protocol boundary; this module never performs I/O or mutates inputs.
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Debug, Clone, Default)]
pub struct ScheduleContext {
    pub warm_nodes: HashMap<String, HashSet<String>>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub job_id: String,
    pub node_id: String,
    pub score: f64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ScheduleResult {
    pub assignments: Vec<Assignment>,
    pub pending: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct Node<'a> {
    raw: &'a Value,
    id: &'a str,
    name: &'a str,
    cpus: f64,
    memory: f64,
    jobs: usize,
    groups: HashMap<String, usize>,
}

fn field<'a>(job: &'a Value, key: &str) -> &'a Value {
    job.get(key).unwrap_or_else(|| &job["spec"][key])
}
fn number(value: &Value, fallback: f64) -> f64 {
    value.as_f64().filter(|n| n.is_finite()).unwrap_or(fallback)
}
fn strings(value: &Value) -> impl Iterator<Item = &str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}
fn request(job: &Value) -> (f64, f64) {
    let r = field(job, "resources");
    (number(&r["cpus"], 2.0), number(&r["memoryMb"], 4096.0))
}

/// Total execution capacity less the operator's host reservation (not current allocations).
pub fn allocatable(node: &Value) -> (f64, f64) {
    if !node["info"].is_object() {
        return (0.0, 0.0);
    }
    let host_cpu = number(&node["info"]["cpus"], 0.0);
    let host_memory = number(&node["info"]["memoryTotalMb"], 0.0);
    let capacity_cpu = host_cpu.min(number(&node["info"]["executionCapacity"]["cpus"], host_cpu));
    let capacity_memory = host_memory.min(number(
        &node["info"]["executionCapacity"]["memoryTotalMb"],
        host_memory,
    ));
    (
        (capacity_cpu - number(&node["settings"]["reserved"]["cpus"], 0.0))
            .max(0.0)
            .min(number(
                &node["info"]["localPolicy"]["budget"]["cpus"],
                f64::MAX,
            )),
        (capacity_memory - number(&node["settings"]["reserved"]["memoryMb"], 0.0))
            .max(0.0)
            .min(number(
                &node["info"]["localPolicy"]["budget"]["memoryMb"],
                f64::MAX,
            )),
    )
}

fn format_request(cpus: f64, memory: f64) -> String {
    let gib = memory / 1024.0;
    let mem = if gib >= 10.0 || gib.fract() == 0.0 {
        format!("{:.0}", gib.round())
    } else {
        format!("{:.1}", (gib * 10.0).round() / 10.0)
    };
    // JavaScript uses exponential notation below 1e-6 and from 1e21. CPU requests are
    // finite positive numbers, but can be fractional enough to reach the lower boundary.
    let cpu = if cpus != 0.0 && !(1e-6..1e21).contains(&cpus.abs()) {
        let scientific = format!("{cpus:e}");
        let (mantissa, exponent) = scientific.split_once('e').unwrap();
        let exponent: i32 = exponent.parse().unwrap();
        format!("{mantissa}e{exponent:+}")
    } else {
        cpus.to_string()
    };
    format!("{cpu} CPU, {mem} GiB")
}

// Stages intentionally rank how close each node came to accepting the job. Pending reasons
// describe the closest eligible candidate rather than an unrelated offline/target mismatch.
fn rejection(job: &Value, node: &Node<'_>) -> Option<(u8, String)> {
    let raw = node.raw;
    if raw["info"]["localPolicy"]["paused"] == true {
        return Some((2, format!("node {} is locally paused", node.name)));
    }
    if let Some(target) = field(job, "target")["node"].as_str()
        && target != node.id
        && target != node.name
    {
        return Some((0, format!("job is pinned to node \"{target}\"")));
    }
    if strings(&job["excludedNodes"]).any(|id| id == node.id) {
        return Some((3, format!("node {} rejected this job", node.name)));
    }
    let online = raw["online"].as_bool().unwrap_or(raw["status"] == "online");
    if !online || !raw["info"].is_object() {
        return Some((1, format!("node {} is offline", node.name)));
    }
    let mode = raw["settings"]["schedulingMode"]
        .as_str()
        .unwrap_or("enabled");
    if mode != "enabled" {
        return Some((2, format!("node {} is {mode}", node.name)));
    }
    let kind = field(job, "kind").as_str().unwrap_or("command");
    if !strings(&raw["info"]["executors"]).any(|k| k == kind) {
        let docker = if raw["info"]["docker"]["available"] == true {
            ""
        } else {
            " (Docker unavailable)"
        };
        return Some((
            3,
            format!("node {} cannot run \"{kind}\" jobs{docker}", node.name),
        ));
    }
    let labels: HashSet<_> = strings(&raw["effectiveLabels"]).collect();
    let missing: Vec<_> = strings(&field(job, "target")["labels"])
        .filter(|label| !labels.contains(label))
        .collect();
    if !missing.is_empty() {
        return Some((
            4,
            format!("node {} lacks label(s) {}", node.name, missing.join(", ")),
        ));
    }
    let (cpu, mem) = request(job);
    let (cap_cpu, cap_mem) = allocatable(raw);
    if cpu > cap_cpu || mem > cap_mem {
        return Some((
            5,
            format!(
                "job needs {} but node {} allows at most {}",
                format_request(cpu, mem),
                node.name,
                format_request(cap_cpu, cap_mem)
            ),
        ));
    }
    let max_jobs = raw["settings"]["maxConcurrency"].as_u64().unwrap_or(4).min(
        raw["info"]["localPolicy"]["budget"]["maxConcurrency"]
            .as_u64()
            .unwrap_or(256),
    ) as usize;
    if node.jobs >= max_jobs {
        return Some((
            6,
            format!(
                "node {} is at its concurrency limit ({max_jobs})",
                node.name
            ),
        ));
    }
    if node.cpus + cpu > cap_cpu || node.memory + mem > cap_mem {
        return Some((
            6,
            format!(
                "waiting for {} on node {}",
                format_request(cpu, mem),
                node.name
            ),
        ));
    }
    if let Some(available) = raw["metrics"]["memoryAvailableMb"].as_f64()
        && available < mem
    {
        return Some((
            6,
            format!(
                "node {} has only {:.0} GiB memory available right now",
                node.name,
                (available / 1024.0).floor()
            ),
        ));
    }
    None
}

fn group(job: &Value) -> Option<&str> {
    job["group"].as_str().or_else(|| job["groupId"].as_str())
}

fn score(job: &Value, node: &Node<'_>, context: &ScheduleContext) -> f64 {
    let (cpu, mem) = request(job);
    let (cap_cpu, cap_mem) = allocatable(node.raw);
    let free_cpu = if cap_cpu > 0.0 {
        (cap_cpu - node.cpus - cpu) / cap_cpu
    } else {
        0.0
    };
    let free_mem = if cap_mem > 0.0 {
        (cap_mem - node.memory - mem) / cap_mem
    } else {
        0.0
    };
    let utilization = number(&node.raw["metrics"]["cpuPercent"], 50.0) / 100.0;
    let warm = field(job, "project")
        .as_str()
        .and_then(|p| context.warm_nodes.get(p))
        .is_some_and(|nodes| nodes.contains(node.id));
    let preferred =
        strings(&field(job, "target")["prefer"]).any(|p| p == node.id || p == node.name);
    let siblings = group(job)
        .and_then(|g| node.groups.get(g))
        .copied()
        .unwrap_or(0);
    free_cpu + free_mem - 0.5 * utilization - 0.1 * node.jobs as f64
        + if warm { 0.5 } else { 0.0 }
        + if preferred { 2.0 } else { 0.0 }
        - 1.5 * siblings as f64
}

fn pending_reason(job: &Value, nodes: &[Node<'_>], rejections: &[(u8, String)]) -> String {
    if nodes.is_empty() {
        return "no worker nodes are registered".into();
    }
    let target = field(job, "target")["node"].as_str();
    if let Some(target) = target
        && !nodes.iter().any(|n| n.id == target || n.name == target)
    {
        return format!("node \"{target}\" does not exist");
    }
    let closest = rejections.iter().filter(|r| r.0 != 0).map(|r| r.0).max();
    let Some(stage) = closest else {
        return "no eligible nodes".into();
    };
    let at_stage: Vec<_> = rejections.iter().filter(|r| r.0 == stage).collect();
    if stage == 1 && target.is_none() {
        return "no worker nodes are online".into();
    }
    if at_stage.len() > 1 {
        match stage {
            6 => {
                let (cpu, mem) = request(job);
                return format!("waiting for capacity ({})", format_request(cpu, mem));
            }
            5 => {
                let (cpu, mem) = request(job);
                return format!("no node is large enough for {}", format_request(cpu, mem));
            }
            4 => {
                return format!(
                    "no eligible node has label(s) {}",
                    strings(&field(job, "target")["labels"])
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            2 => return "all eligible nodes are disabled or draining".into(),
            _ => {}
        }
    }
    at_stage[0].1.clone()
}

/// Place jobs in descending priority, FIFO order with backfill. Reservations made earlier in
/// this pass immediately affect later jobs. Preferences and shard spreading are score terms.
pub fn schedule(queue: &[Value], nodes: &[Value], context: &ScheduleContext) -> ScheduleResult {
    let mut working: Vec<_> = nodes
        .iter()
        .map(|raw| Node {
            raw,
            id: raw["id"].as_str().unwrap_or(""),
            name: raw["name"]
                .as_str()
                .or_else(|| raw["id"].as_str())
                .unwrap_or(""),
            cpus: number(&raw["allocated"]["cpus"], 0.0),
            memory: number(&raw["allocated"]["memoryMb"], 0.0),
            jobs: raw["allocated"]["jobs"]
                .as_u64()
                .map(|n| n as usize)
                .unwrap_or_else(|| raw["activeJobs"].as_array().map_or(0, Vec::len)),
            groups: raw["groups"]
                .as_object()
                .map(|groups| {
                    groups
                        .iter()
                        .filter_map(|(g, count)| count.as_u64().map(|n| (g.clone(), n as usize)))
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect();
    let mut ordered: Vec<_> = queue.iter().collect();
    ordered.sort_by(|a, b| {
        number(field(b, "priority"), 0.0)
            .total_cmp(&number(field(a, "priority"), 0.0))
            .then_with(|| number(&a["queuedAt"], 0.0).total_cmp(&number(&b["queuedAt"], 0.0)))
            .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let mut result = ScheduleResult::default();
    for job in ordered {
        let mut best: Option<(usize, f64)> = None;
        let mut reasons = Vec::new();
        for (index, node) in working.iter().enumerate() {
            if let Some(reason) = rejection(job, node) {
                reasons.push(reason);
                continue;
            }
            let candidate = score(job, node, context);
            if best.is_none_or(|(old, old_score)| {
                candidate > old_score
                    || (candidate == old_score
                        && node
                            .name
                            .encode_utf16()
                            .cmp(working[old].name.encode_utf16())
                            .is_lt())
            }) {
                best = Some((index, candidate));
            }
        }
        let job_id = job["id"].as_str().unwrap_or("").to_owned();
        if let Some((index, score)) = best {
            let node = &mut working[index];
            let (cpu, mem) = request(job);
            node.cpus += cpu;
            node.memory += mem;
            node.jobs += 1;
            if let Some(g) = group(job) {
                *node.groups.entry(g.to_owned()).or_default() += 1;
            }
            result.assignments.push(Assignment {
                job_id,
                node_id: node.id.to_owned(),
                score,
            });
        } else {
            result
                .pending
                .insert(job_id, pending_reason(job, &working, &reasons));
        }
    }
    result
}
