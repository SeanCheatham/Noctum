use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecRequest {
    pub cwd: PathBuf,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub timeout_secs: u64,
}

#[derive(Serialize, Deserialize)]
pub struct Reply<T> {
    pub value: Option<T>,
    pub error: Option<String>,
}
