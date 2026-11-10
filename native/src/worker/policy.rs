//! Durable local intent. Controller settings are never written into this file.
use crate::state;
use anyhow::{Context, Result, bail, ensure};
use gridstator_local_control_protocol::{Policy, VERSION};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

pub(crate) struct PolicyStore {
    path: PathBuf,
    /// Admission and policy mutation use this mutex before locking the active-job map.
    pub(crate) value: Mutex<Policy>,
}

impl PolicyStore {
    pub(crate) fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("local-policy.json");
        let value = load(&path)?;
        Ok(Self {
            path,
            value: Mutex::new(value),
        })
    }
    pub(crate) fn snapshot(&self) -> Policy {
        self.value.lock().unwrap().clone()
    }
    pub(crate) fn update(&self, action: &str, patch: &Value) -> Result<Policy> {
        let mut guard = self.value.lock().unwrap();
        let next = update_file(&self.path, action, patch)?;
        *guard = next.clone();
        Ok(next)
    }
}

pub(crate) fn load(path: &Path) -> Result<Policy> {
    if !path.exists() {
        return Ok(Policy::default());
    }
    crate::local_worker::owned_regular_file(path)?;
    let policy: Policy = serde_json::from_value(state::read_json(path)?)
        .context("CONFIG: invalid local worker policy")?;
    ensure!(
        policy.version == VERSION,
        "CONFIG: unsupported local worker policy"
    );
    policy.budget.validate().map_err(anyhow::Error::msg)?;
    Ok(policy)
}

pub(crate) fn update_file(path: &Path, action: &str, patch: &Value) -> Result<Policy> {
    if path.exists() {
        crate::local_worker::owned_regular_file(path)?;
    }
    let next = state::update_json(path, |saved| {
        let mut policy: Policy = if saved.as_object().is_some_and(|v| v.is_empty()) {
            Policy::default()
        } else {
            serde_json::from_value(saved.clone()).context("CONFIG: invalid local worker policy")?
        };
        ensure!(
            policy.version == VERSION,
            "CONFIG: unsupported local worker policy"
        );
        match action {
            "pause" => policy.paused = true,
            "resume" => policy.paused = false,
            "setStartup" => {
                policy.start_at_sign_in = patch["startAtSignIn"]
                    .as_bool()
                    .context("startAtSignIn must be boolean")?
            }
            "setBudget" => {
                let patch = patch["budget"]
                    .as_object()
                    .context("budget must be an object")?;
                let mut budget = serde_json::to_value(&policy.budget)?;
                for (key, value) in patch {
                    ensure!(
                        ["cpus", "memoryMb", "maxConcurrency"].contains(&key.as_str()),
                        "unknown budget field"
                    );
                    budget[key] = value.clone();
                }
                policy.budget = serde_json::from_value(budget).context("invalid local budget")?;
            }
            _ => bail!("unsupported local policy action"),
        }
        policy.budget.validate().map_err(anyhow::Error::msg)?;
        policy.generation = policy
            .generation
            .checked_add(1)
            .context("local policy generation exhausted")?;
        *saved = serde_json::to_value(&policy)?;
        Ok(())
    })?;
    Ok(serde_json::from_value(next)?)
}

pub(crate) fn request_resources(assignment: &Value) -> (f64, u64) {
    (
        assignment["spec"]["resources"]["cpus"]
            .as_f64()
            .unwrap_or(2.0),
        assignment["spec"]["resources"]["memoryMb"]
            .as_u64()
            .unwrap_or(4096),
    )
}

pub(crate) fn admission(
    policy: &Policy,
    controller: &Value,
    active: &[Value],
    assignment: &Value,
) -> Result<()> {
    ensure!(!policy.paused, "worker is locally paused");
    ensure!(
        !matches!(
            controller["schedulingMode"].as_str(),
            Some("disabled" | "draining")
        ),
        "controller has paused this worker"
    );
    let maximum = policy
        .budget
        .max_concurrency
        .unwrap_or(256)
        .min(controller["maxConcurrency"].as_u64().unwrap_or(256) as u32)
        as usize;
    ensure!(
        active.len() < maximum,
        "worker local concurrency budget is full"
    );
    let (cpu, memory) = request_resources(assignment);
    ensure!(
        cpu.is_finite() && cpu > 0.0 && memory > 0,
        "invalid assignment resources"
    );
    let (used_cpu, used_memory) = active.iter().map(request_resources).fold(
        (0.0, 0_u64),
        |(cpu, memory), (next_cpu, next_memory)| {
            (cpu + next_cpu, memory.saturating_add(next_memory))
        },
    );
    if let Some(limit) = policy.budget.cpus {
        ensure!(
            used_cpu + cpu <= limit + 1e-9,
            "worker local CPU budget is full"
        );
    }
    if let Some(limit) = policy.budget.memory_mb {
        ensure!(
            used_memory.saturating_add(memory) <= limit,
            "worker local memory budget is full"
        );
    }
    Ok(())
}

pub(crate) fn advertise(info: &Value, policy: &Policy) -> Value {
    let mut info = info.clone();
    info["localPolicy"] = json!({"version":VERSION,"generation":policy.generation,"paused":policy.paused,"budget":policy.budget});
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pause_and_budgets_survive_reload_and_cannot_be_relaxed_by_controller() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = PolicyStore::open(dir.path())?;
        store.update("pause", &json!({}))?;
        store.update(
            "setBudget",
            &json!({"budget":{"cpus":2.0,"memoryMb":4096,"maxConcurrency":1}}),
        )?;
        let reloaded = PolicyStore::open(dir.path())?;
        let assignment = json!({"spec":{"resources":{"cpus":1,"memoryMb":1024}}});
        assert!(
            admission(
                &reloaded.snapshot(),
                &json!({"maxConcurrency":256}),
                &[],
                &assignment
            )
            .is_err()
        );
        reloaded.update("resume", &json!({}))?;
        assert!(
            admission(
                &reloaded.snapshot(),
                &json!({"maxConcurrency":256}),
                std::slice::from_ref(&assignment),
                &assignment
            )
            .is_err()
        );
        assert!(
            admission(
                &reloaded.snapshot(),
                &json!({"schedulingMode":"disabled"}),
                &[],
                &assignment
            )
            .is_err()
        );
        assert!(admission(&reloaded.snapshot(), &json!({}), &[], &assignment).is_ok());
        Ok(())
    }
    #[test]
    fn invalid_budget_never_changes_saved_policy() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = PolicyStore::open(dir.path())?;
        let before = store.update("setBudget", &json!({"budget":{"cpus":2.5}}))?;
        assert!(
            store
                .update("setBudget", &json!({"budget":{"maxConcurrency":0}}))
                .is_err()
        );
        assert_eq!(store.snapshot(), before);
        assert_eq!(load(&dir.path().join("local-policy.json"))?, before);
        Ok(())
    }
}
