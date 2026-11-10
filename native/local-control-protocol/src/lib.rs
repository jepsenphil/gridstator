//! Portable desktop/worker protocol. This crate has no GUI, OS, or core runtime dependency.
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const VERSION: u32 = 1;
pub const CAPABILITY: &str = "worker-control-v1";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Budget {
    pub cpus: Option<f64>,
    pub memory_mb: Option<u64>,
    pub max_concurrency: Option<u32>,
}

impl Budget {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .cpus
            .is_some_and(|value| !value.is_finite() || value <= 0.0 || value > 65536.0)
        {
            return Err("CPU budget must be finite and greater than zero");
        }
        if self
            .memory_mb
            .is_some_and(|value| value == 0 || value > 1_099_511_627_776)
        {
            return Err("Memory budget must be greater than zero");
        }
        if self
            .max_concurrency
            .is_some_and(|value| value == 0 || value > 256)
        {
            return Err("Concurrency must be between 1 and 256");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Policy {
    pub version: u32,
    pub generation: u64,
    pub paused: bool,
    pub budget: Budget,
    pub start_at_sign_in: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            version: VERSION,
            generation: 1,
            paused: false,
            budget: Budget::default(),
            start_at_sign_in: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub name: String,
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerSnapshot {
    pub data_dir: String,
    pub node_id: String,
    pub cluster_id: Option<String>,
    pub ca_fingerprint: String,
    pub name: String,
    pub controller_url: String,
    pub embedded: bool,
    #[serde(default)]
    pub controller_running: bool,
    pub running: bool,
    pub connected: bool,
    pub state: String,
    pub paused: bool,
    pub draining: bool,
    pub start_at_sign_in: bool,
    pub budget: Budget,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_budget: Option<Budget>,
    pub generation: u64,
    pub active_jobs: Vec<Job>,
    pub recent_jobs: Vec<Job>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_settings: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerResponse {
    pub version: u32,
    pub workers: Vec<WorkerSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<LogPage>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogPage {
    pub job_id: String,
    pub entries: Vec<Value>,
    pub next_seq: u64,
}
