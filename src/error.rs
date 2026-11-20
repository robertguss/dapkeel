use serde::Serialize;
use thiserror::Error;
use url::Url;

#[derive(Debug, Error)]
pub enum DapError {
    #[error("{message}")]
    Coded {
        code: &'static str,
        message: String,
        fix_hint: Option<String>,
        exit_code: i32,
        detail: Option<serde_json::Value>,
    },
}

#[derive(Debug, Serialize)]
pub struct ErrorPayload {
    pub ok: bool,
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

impl DapError {
    pub fn config(message: impl Into<String>, fix_hint: impl Into<String>) -> Self {
        Self::Coded {
            code: "DAP_CONFIG_ERROR",
            message: message.into(),
            fix_hint: Some(fix_hint.into()),
            exit_code: 2,
            detail: None,
        }
    }

    pub fn auth(message: impl Into<String>, fix_hint: impl Into<String>) -> Self {
        Self::Coded {
            code: "DAP_AUTH_FAILED",
            message: message.into(),
            fix_hint: Some(fix_hint.into()),
            exit_code: 1,
            detail: None,
        }
    }

    pub fn table_access_denied(namespace: &str, table: &str) -> Self {
        Self::Coded {
            code: "DAP_TABLE_ACCESS_DENIED",
            message: format!(
                "Token issuer did not grant access to '{namespace}/{table}' in authorization_details"
            ),
            fix_hint: Some(
                "Verify your DAP API key permissions in Instructure Identity Service include this namespace and table."
                    .to_string(),
            ),
            exit_code: 1,
            detail: Some(serde_json::json!({ "namespace": namespace, "table": table })),
        }
    }

    pub fn snapshot_required(message: impl Into<String>) -> Self {
        Self::Coded {
            code: "DAP_SNAPSHOT_REQUIRED",
            message: message.into(),
            fix_hint: Some(
                "The incremental window is too old for DAP retention; run a fresh `dapkeel snapshot` (or `dropdb` then `initdb`)."
                    .to_string(),
            ),
            exit_code: 1,
            detail: None,
        }
    }

    pub fn api(
        message: impl Into<String>,
        fix_hint: Option<String>,
        detail: Option<serde_json::Value>,
    ) -> Self {
        Self::Coded {
            code: "DAP_API_ERROR",
            message: message.into(),
            fix_hint,
            exit_code: 1,
            detail,
        }
    }

    pub fn job_failed(job_id: &str, message: impl Into<String>) -> Self {
        Self::Coded {
            code: "DAP_JOB_FAILED",
            message: format!("DAP job {job_id} failed: {}", message.into()),
            fix_hint: Some(
                "Check the requested table, format, or timestamp range and retry.".to_string(),
            ),
            exit_code: 1,
            detail: Some(serde_json::json!({ "job_id": job_id })),
        }
    }

    pub fn download(message: impl Into<String>) -> Self {
        Self::Coded {
            code: "DAP_DOWNLOAD_ERROR",
            message: message.into(),
            fix_hint: Some(
                "Check network connectivity and retry; pre-signed URLs expire after ~15 minutes."
                    .to_string(),
            ),
            exit_code: 1,
            detail: None,
        }
    }

    pub fn schema(message: impl Into<String>) -> Self {
        Self::Coded {
            code: "DAP_SCHEMA_ERROR",
            message: message.into(),
            fix_hint: Some(
                "Inspect the returned DAP JSON schema for unexpected types.".to_string(),
            ),
            exit_code: 1,
            detail: None,
        }
    }

    pub fn db(message: impl Into<String>, fix_hint: Option<String>) -> Self {
        Self::Coded {
            code: "DAP_DB_ERROR",
            message: message.into(),
            fix_hint,
            exit_code: 1,
            detail: None,
        }
    }

    pub fn already_initialized(namespace: &str, table: &str) -> Self {
        Self::Coded {
            code: "DAP_ALREADY_INITIALIZED",
            message: format!("Table '{namespace}.{table}' is already initialized in table_sync"),
            fix_hint: Some(format!(
                "Run `dapkeel syncdb --namespace {namespace} --table {table}` to apply incremental updates, or `dropdb` first to re-initialize."
            )),
            exit_code: 1,
            detail: Some(serde_json::json!({ "namespace": namespace, "table": table })),
        }
    }

    pub fn not_initialized(namespace: &str, table: &str) -> Self {
        Self::Coded {
            code: "DAP_NOT_INITIALIZED",
            message: format!("Table '{namespace}.{table}' is not initialized in table_sync"),
            fix_hint: Some(format!(
                "Run `dapkeel initdb --namespace {namespace} --table {table}` first."
            )),
            exit_code: 1,
            detail: Some(serde_json::json!({ "namespace": namespace, "table": table })),
        }
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self::Coded {
            code: "DAP_IO_ERROR",
            message: message.into(),
            fix_hint: Some("Verify file paths and write permissions.".to_string()),
            exit_code: 1,
            detail: None,
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Coded { code, .. } => code,
        }
    }

    pub fn fix_hint(&self) -> Option<&str> {
        match self {
            Self::Coded { fix_hint, .. } => fix_hint.as_deref(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Coded { exit_code, .. } => *exit_code,
        }
    }

    pub fn to_payload(&self) -> ErrorPayload {
        match self {
            Self::Coded {
                code,
                message,
                fix_hint,
                detail,
                ..
            } => ErrorPayload {
                ok: false,
                error: ErrorBody {
                    code: (*code).to_string(),
                    message: message.clone(),
                    fix_hint: fix_hint.clone(),
                    detail: detail.clone(),
                },
            },
        }
    }
}

/// Redact password from a PostgreSQL URL so credentials are never printed or logged.
pub fn redact_url_password(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(mut parsed) => {
            if parsed.password().is_some() {
                let _ = parsed.set_password(Some("***"));
            }
            parsed.to_string()
        }
        Err(_) => "<redacted-url>".to_string(),
    }
}
