use rmcp::schemars;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub chip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chip_model: Option<String>,
    pub mode: String,
    pub vid: String,
    pub pid: String,
    pub connection: String,
    pub capacity: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionInfo {
    pub name: String,
    pub start_sector: u64,
    pub sectors: u64,
    pub size: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwarePartition {
    pub name: String,
    pub size: u64,
    pub start_sector: Option<u64>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareInfo {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub kind: String,
    pub has_embedded_loader: bool,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
    pub partitions: Vec<FirmwarePartition>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationKind {
    Flash,
    Backup,
    Erase,
    Loader,
    Reboot,
    Maskrom,
    Unpack,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperationRequest {
    pub device_id: String,
    pub kind: OperationKind,
    pub firmware_path: Option<String>,
    pub output_path: Option<String>,
    pub start_sector: u64,
    pub sectors: Option<u64>,
    // Accept the removed option in older requests without affecting execution.
    #[serde(default, skip_serializing, rename = "verify")]
    pub _legacy_verify: Option<bool>,
    pub reboot: bool,
    pub confirmed: bool,
    pub writes: Option<Vec<WriteEntry>>,
    pub archive_member: Option<String>,
    pub partition_name: Option<String>,
}

#[derive(Clone, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WriteEntry {
    /// A label for this RAW image.
    pub name: String,
    /// Absolute local image path.
    pub path: String,
    /// Zero-based LBA in 512-byte sectors.
    pub start_sector: u64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationEvent {
    pub job_id: String,
    pub status: String,
    pub stage: String,
    pub progress: Option<f64>,
    pub message: String,
    pub level: String,
    pub timestamp: String,
    pub bytes_done: Option<u64>,
    pub bytes_total: Option<u64>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub device: String,
    pub operation: OperationKind,
    pub file: Option<String>,
    pub status: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub message: String,
}

#[derive(Serialize)]
pub struct RuntimeInfo {
    pub version: String,
    pub platform: String,
    pub backend: String,
}
