use crate::error::DapError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatermarkStore {
    pub tables: BTreeMap<String, TableWatermark>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableWatermark {
    pub namespace: String,
    pub table: String,
    pub timestamp: DateTime<Utc>,
    pub schema_version: i32,
    pub job_id: String,
    pub updated_at: DateTime<Utc>,
}

impl WatermarkStore {
    pub fn key(namespace: &str, table: &str) -> String {
        format!("{namespace}.{table}")
    }

    pub fn load(path: &Path) -> Result<Self, DapError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path).map_err(|e| {
            DapError::io(format!(
                "Failed to read state file '{}': {e}",
                path.display()
            ))
        })?;
        serde_json::from_str(&raw).map_err(|e| {
            DapError::io(format!(
                "Failed to parse state file '{}': {e}",
                path.display()
            ))
        })
    }

    pub fn get(&self, namespace: &str, table: &str) -> Option<&TableWatermark> {
        self.tables.get(&Self::key(namespace, table))
    }

    pub fn set(
        &mut self,
        namespace: &str,
        table: &str,
        timestamp: DateTime<Utc>,
        schema_version: i32,
        job_id: &str,
    ) {
        self.tables.insert(
            Self::key(namespace, table),
            TableWatermark {
                namespace: namespace.to_string(),
                table: table.to_string(),
                timestamp,
                schema_version,
                job_id: job_id.to_string(),
                updated_at: Utc::now(),
            },
        );
    }

    /// Persist atomically via temp file + rename so partial failures never corrupt watermarks.
    pub fn save_atomic(&self, path: &Path) -> Result<(), DapError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|e| {
                DapError::io(format!(
                    "Failed to create state dir '{}': {e}",
                    parent.display()
                ))
            })?;
        }
        let tmp_name = format!(
            ".{}.tmp.{}",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("dap_state"),
            std::process::id()
        );
        let tmp_path = path.with_file_name(tmp_name);
        let payload = serde_json::to_string_pretty(self)
            .map_err(|e| DapError::io(format!("Failed serializing watermark state: {e}")))?;
        fs::write(&tmp_path, payload).map_err(|e| {
            DapError::io(format!(
                "Failed writing temp state file '{}': {e}",
                tmp_path.display()
            ))
        })?;
        fs::rename(&tmp_path, path).map_err(|e| {
            DapError::io(format!(
                "Failed renaming state file to '{}': {e}",
                path.display()
            ))
        })?;
        Ok(())
    }
}
