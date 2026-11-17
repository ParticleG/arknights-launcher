use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Settings {
    pub game_dir: String,
    pub compat_data: String,
    pub proton_path: String,
}

#[derive(Clone, Serialize)]
pub struct GameStatus {
    pub installed: bool,
    pub running: bool,
    pub verified_version: Option<String>,
    pub settings: Settings,
}

#[derive(Clone, Serialize)]
pub struct ReleaseSummary {
    pub version: String,
    pub client_version: String,
    pub file_count: usize,
    pub compressed_bytes: u64,
    pub installed_bytes: u64,
}

#[derive(Serialize)]
pub struct LaunchResult {
    pub already_running: bool,
}

#[derive(Clone, Serialize)]
pub struct VerificationSummary {
    pub version: String,
    pub files_checked: usize,
    pub missing_files: usize,
    pub changed_files: usize,
    pub verified_bytes: u64,
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Verify,
    Install,
}

#[derive(Clone, Serialize)]
pub struct OperationProgress {
    pub operation: Operation,
    pub phase: String,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub completed_files: usize,
    pub total_files: usize,
    pub current_file: String,
}

#[derive(Clone, Serialize)]
pub struct OperationFinished {
    pub operation: Operation,
    pub cancelled: bool,
    pub summary: Option<VerificationSummary>,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct GameExited {
    pub exit_code: Option<i32>,
}
