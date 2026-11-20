use chrono::{DateTime, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DataFormat {
    Jsonl,
    Csv,
    Tsv,
    Parquet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum DataMode {
    Expanded,
    Condensed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ContractFormat {
    Odcs,
    Toml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// Try DAP v3 /oauth/token first; automatically fall back to DAP v2 /ids/auth/login on 401/404
    Auto,
    /// Strict DAP v3 OAuth 2.0 (/oauth/token with per-table authorization_details)
    Oauth,
    /// Legacy DAP v2 Identity Service (/ids/auth/login)
    Ids,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NamespaceList {
    pub namespaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableList {
    pub tables: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionedSchema {
    pub schema: serde_json::Value,
    pub version: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotQueryRequest {
    pub format: DataFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<DataMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncrementalQueryRequest {
    pub format: DataFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<DataMode>,
    pub since: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Waiting,
    Running,
    Complete,
    Failed,
}

impl JobStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Complete | Self::Failed)
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DapObject {
    pub id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobError {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResponse {
    pub id: String,
    pub status: JobStatus,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub objects: Option<Vec<DapObject>>,
    #[serde(default)]
    pub schema_version: Option<i32>,
    #[serde(default)]
    pub at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub until: Option<DateTime<Utc>>,
    #[serde(default)]
    pub error: Option<JobError>,
}

impl JobResponse {
    /// Returns the watermark timestamp (`at` for snapshot jobs, `until` for incremental jobs).
    pub fn watermark(&self) -> Option<DateTime<Utc>> {
        self.at.or(self.until)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceEntry {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceResult {
    pub urls: HashMap<String, ResourceEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub scope: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub authorization_details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthErrorBody {
    pub error: String,
    #[serde(default)]
    pub error_description: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DapApiErrorEnvelope {
    pub error: DapApiErrorInner,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DapApiErrorInner {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}
