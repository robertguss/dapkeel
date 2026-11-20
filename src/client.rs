use crate::error::DapError;
use crate::types::{
    DapApiErrorEnvelope, DapObject, DataFormat, DataMode, IncrementalQueryRequest, JobResponse,
    JobStatus, NamespaceList, OAuthErrorBody, ResourceEntry, ResourceResult, SnapshotQueryRequest,
    TableList, TokenResponse, VersionedSchema,
};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use flate2::write::MultiGzDecoder;
use futures_util::StreamExt;
use reqwest::header::{
    AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue, RETRY_AFTER, USER_AGENT,
};
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};
use url::Url;
use uuid::Uuid;

const EXPIRE_AT_VERY_CLOSE_SECS: i64 = 30;
const TOKEN_REFRESH_BUFFER_SECS: i64 = 300;
const MAX_HTTP_RETRIES: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableAccess {
    pub namespace: Option<String>,
    pub table: Option<String>,
}

impl TableAccess {
    pub fn none() -> Self {
        Self {
            namespace: None,
            table: None,
        }
    }

    pub fn for_table(namespace: &str, table: &str) -> Self {
        Self {
            namespace: Some(namespace.to_string()),
            table: Some(table.to_string()),
        }
    }
}

#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    expires_at: DateTime<Utc>,
    access: TableAccess,
}

impl CachedToken {
    fn is_expiring(&self) -> bool {
        self.expires_at < Utc::now() + ChronoDuration::seconds(TOKEN_REFRESH_BUFFER_SECS)
    }
}

#[derive(Clone)]
pub struct DapClient {
    base_url: String,
    basic_credentials: String,
    api_http: Client,
    download_http: Client,
    token_cache: Arc<Mutex<Option<CachedToken>>>,
    poll_interval: Duration,
    download_concurrency: usize,
}

impl DapClient {
    pub fn new(
        base_url: String,
        client_id: String,
        client_secret: String,
        poll_interval_ms: u64,
        download_concurrency: usize,
    ) -> Result<Self, DapError> {
        let base_url = base_url.trim().trim_end_matches('/').to_string();
        if base_url.is_empty() {
            return Err(DapError::config(
                "Missing DAP API base URL",
                "Set DAP_API_URL (e.g. https://us-east-1.api-gateway.instructure.com) or pass --api-url.",
            ));
        }
        if client_id.trim().is_empty() {
            return Err(DapError::config(
                "Missing DAP client ID",
                "Set DAP_CLIENT_ID or pass --client-id (obtained from identity.instructure.com).",
            ));
        }
        if client_secret.trim().is_empty() {
            return Err(DapError::config(
                "Missing DAP client secret",
                "Set DAP_CLIENT_SECRET or pass --client-secret (obtained from identity.instructure.com).",
            ));
        }

        let trace_id = Uuid::new_v4().simple().to_string();
        let traceparent = format!("00-{trace_id}-0000000000000001-00");
        let ua = format!("dapkeel/{}", env!("CARGO_PKG_VERSION"));

        let mut default_headers = HeaderMap::new();
        default_headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&ua).unwrap_or_else(|_| HeaderValue::from_static("dapkeel")),
        );
        if let Ok(tp) = HeaderValue::from_str(&traceparent) {
            default_headers.insert("traceparent", tp);
        }

        let api_http = Client::builder()
            .default_headers(default_headers.clone())
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(30 * 60))
            .build()
            .map_err(|e| DapError::config(format!("Failed to build HTTP client: {e}"), ""))?;

        // Separate client for pre-signed cloud storage URLs: never carries Bearer tokens.
        let download_http = Client::builder()
            .default_headers(default_headers)
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(30 * 60))
            .build()
            .map_err(|e| {
                DapError::config(format!("Failed to build download HTTP client: {e}"), "")
            })?;

        let basic_credentials = STANDARD.encode(format!("{client_id}:{client_secret}"));

        Ok(Self {
            base_url,
            basic_credentials,
            api_http,
            download_http,
            token_cache: Arc::new(Mutex::new(None)),
            poll_interval: Duration::from_millis(poll_interval_ms.max(10)),
            download_concurrency: download_concurrency.max(1),
        })
    }

    async fn authenticate(&self, access: TableAccess) -> Result<String, DapError> {
        let mut guard = self.token_cache.lock().await;
        if let Some(cached) = guard.as_ref()
            && !cached.is_expiring()
            && (cached.access == access || access.table.is_none())
        {
            return Ok(cached.token.clone());
        }

        // Clear stale token before requesting a fresh one.
        *guard = None;

        let mut form: Vec<(&str, String)> = vec![
            ("grant_type", "client_credentials".to_string()),
            ("scope", "dap".to_string()),
        ];

        if let (Some(ns), Some(tbl)) = (&access.namespace, &access.table) {
            let auth_details = serde_json::json!([{
                "type": "dap",
                "namespaces": { ns: [tbl] }
            }]);
            form.push(("authorization_details", auth_details.to_string()));
        }

        let url = format!("{}/oauth/token", self.base_url);
        let resp = self
            .api_http
            .post(&url)
            .header(AUTHORIZATION, format!("Basic {}", self.basic_credentials))
            .form(&form)
            .send()
            .await
            .map_err(|e| {
                DapError::auth(
                    format!("OAuth token request to {url} failed: {e}"),
                    "Check DAP_API_URL and network connectivity.",
                )
            })?;

        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            DapError::auth(
                format!("Failed reading OAuth response body: {e}"),
                "Retry the request or verify DAP_API_URL.",
            )
        })?;

        if !status.is_success() {
            if let Ok(err) = serde_json::from_str::<OAuthErrorBody>(&body) {
                let desc = err
                    .error_description
                    .map(|d| format!(": {d}"))
                    .unwrap_or_default();
                return Err(DapError::auth(
                    format!(
                        "OAuth error '{}' (HTTP {}){}",
                        err.error,
                        status.as_u16(),
                        desc
                    ),
                    "Verify DAP_CLIENT_ID and DAP_CLIENT_SECRET from identity.instructure.com.",
                ));
            }
            return Err(DapError::auth(
                format!(
                    "Authentication failed with HTTP {}: {}",
                    status.as_u16(),
                    truncate(&body, 300)
                ),
                "Verify DAP_API_URL, DAP_CLIENT_ID, and DAP_CLIENT_SECRET.",
            ));
        }

        let tok: TokenResponse = serde_json::from_str(&body).map_err(|e| {
            DapError::auth(
                format!("Invalid OAuth token JSON response: {e}"),
                "Verify DAP_API_URL points to your regional Instructure API Gateway.",
            )
        })?;

        if let (Some(ns), Some(tbl)) = (&access.namespace, &access.table) {
            verify_granted_table_access(ns, tbl, tok.authorization_details.as_ref())?;
        }

        let expires_at = decode_jwt_expiry(&tok.access_token).unwrap_or_else(|| {
            Utc::now() + ChronoDuration::seconds(tok.expires_in.unwrap_or(3600))
        });

        let cached = CachedToken {
            token: tok.access_token.clone(),
            expires_at,
            access,
        };
        *guard = Some(cached);
        Ok(tok.access_token)
    }

    async fn api_request<B: Serialize + ?Sized, R: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        access: TableAccess,
    ) -> Result<R, DapError> {
        let token = self.authenticate(access).await?;
        let url = format!("{}{}", self.base_url, path);

        let mut attempt = 0usize;
        loop {
            let mut req = self
                .api_http
                .request(method.clone(), &url)
                .header(AUTHORIZATION, format!("Bearer {token}"));

            if let Some(payload) = body {
                req = req.header(CONTENT_TYPE, "application/json").json(payload);
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(_) if attempt + 1 < MAX_HTTP_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(backoff_duration(attempt, None)).await;
                    continue;
                }
                Err(e) => {
                    return Err(DapError::api(
                        format!("HTTP {} {} failed: {e}", method, path),
                        Some("Check network connectivity and DAP_API_URL.".to_string()),
                        None,
                    ));
                }
            };

            let status = resp.status();
            let retry_after = resp
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok());
            let text = resp.text().await.unwrap_or_default();

            if (status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error())
                && attempt + 1 < MAX_HTTP_RETRIES
            {
                attempt += 1;
                tokio::time::sleep(backoff_duration(attempt, retry_after)).await;
                continue;
            }

            if !status.is_success() {
                return Err(map_api_error(status, path, &text));
            }

            return serde_json::from_str::<R>(&text).map_err(|e| {
                DapError::api(
                    format!(
                        "Failed to parse DAP JSON response from {}: {e} (body: {})",
                        path,
                        truncate(&text, 240)
                    ),
                    None,
                    None,
                )
            });
        }
    }

    pub async fn list_namespaces(&self) -> Result<Vec<String>, DapError> {
        let resp: NamespaceList = self
            .api_request::<(), _>(
                Method::GET,
                "/dap/query/namespace",
                None,
                TableAccess::none(),
            )
            .await?;
        Ok(resp.namespaces)
    }

    pub async fn list_tables(&self, namespace: &str) -> Result<Vec<String>, DapError> {
        let path = format!("/dap/query/{namespace}/table");
        let resp: TableList = self
            .api_request::<(), _>(Method::GET, &path, None, TableAccess::none())
            .await?;
        Ok(resp.tables)
    }

    pub async fn resolve_tables(
        &self,
        namespace: &str,
        table_arg: &str,
    ) -> Result<Vec<String>, DapError> {
        if table_arg.trim().eq_ignore_ascii_case("all") {
            self.list_tables(namespace).await
        } else {
            let list: Vec<String> = table_arg
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if list.is_empty() {
                return Err(DapError::config(
                    "No table names provided",
                    "Pass --table <table_name>[,<table_2>] or --table all.",
                ));
            }
            Ok(list)
        }
    }

    pub async fn get_schema(
        &self,
        namespace: &str,
        table: &str,
    ) -> Result<VersionedSchema, DapError> {
        let path = format!("/dap/query/{namespace}/table/{table}/schema");
        self.api_request::<(), _>(Method::GET, &path, None, TableAccess::none())
            .await
    }

    pub async fn query_snapshot(
        &self,
        namespace: &str,
        table: &str,
        format: DataFormat,
        mode: Option<DataMode>,
    ) -> Result<JobResponse, DapError> {
        let path = format!("/dap/query/{namespace}/table/{table}/data");
        let req = SnapshotQueryRequest { format, mode };
        self.api_request(
            Method::POST,
            &path,
            Some(&req),
            TableAccess::for_table(namespace, table),
        )
        .await
    }

    pub async fn query_incremental(
        &self,
        namespace: &str,
        table: &str,
        format: DataFormat,
        mode: Option<DataMode>,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
    ) -> Result<JobResponse, DapError> {
        let path = format!("/dap/query/{namespace}/table/{table}/data");
        let req = IncrementalQueryRequest {
            format,
            mode,
            since,
            until,
        };
        self.api_request(
            Method::POST,
            &path,
            Some(&req),
            TableAccess::for_table(namespace, table),
        )
        .await
    }

    pub async fn get_job(
        &self,
        job_id: &str,
        namespace: &str,
        table: &str,
    ) -> Result<JobResponse, DapError> {
        let path = format!("/dap/job/{job_id}");
        self.api_request::<(), _>(
            Method::GET,
            &path,
            None,
            TableAccess::for_table(namespace, table),
        )
        .await
    }

    pub async fn execute_snapshot_job(
        &self,
        namespace: &str,
        table: &str,
        format: DataFormat,
        mode: Option<DataMode>,
    ) -> Result<JobResponse, DapError> {
        let mut job = self.query_snapshot(namespace, table, format, mode).await?;
        if job.status == JobStatus::Complete
            && let Some(exp) = job.expires_at
            && (exp - Utc::now()).num_seconds() < EXPIRE_AT_VERY_CLOSE_SECS
        {
            tokio::time::sleep(Duration::from_secs((EXPIRE_AT_VERY_CLOSE_SECS + 2) as u64)).await;
            job = self.query_snapshot(namespace, table, format, mode).await?;
        }
        self.await_job(job, namespace, table).await
    }

    pub async fn execute_incremental_job(
        &self,
        namespace: &str,
        table: &str,
        format: DataFormat,
        mode: Option<DataMode>,
        since: DateTime<Utc>,
        until: Option<DateTime<Utc>>,
    ) -> Result<JobResponse, DapError> {
        let mut job = self
            .query_incremental(namespace, table, format, mode, since, until)
            .await?;
        if job.status == JobStatus::Complete
            && let Some(exp) = job.expires_at
            && (exp - Utc::now()).num_seconds() < EXPIRE_AT_VERY_CLOSE_SECS
        {
            tokio::time::sleep(Duration::from_secs((EXPIRE_AT_VERY_CLOSE_SECS + 2) as u64)).await;
            job = self
                .query_incremental(namespace, table, format, mode, since, until)
                .await?;
        }
        self.await_job(job, namespace, table).await
    }

    async fn await_job(
        &self,
        mut job: JobResponse,
        namespace: &str,
        table: &str,
    ) -> Result<JobResponse, DapError> {
        while !job.status.is_terminal() {
            tokio::time::sleep(self.poll_interval).await;
            job = self.get_job(&job.id, namespace, table).await?;
        }

        if job.status != JobStatus::Complete {
            let reason = job
                .error
                .as_ref()
                .and_then(|e| e.message.clone())
                .unwrap_or_else(|| format!("status={}", job.status.as_str()));
            return Err(DapError::job_failed(&job.id, reason));
        }

        // Ensure objects list is populated (some gateways only include objects on GET /dap/job/{id})
        if job.objects.is_none() {
            job = self.get_job(&job.id, namespace, table).await?;
        }

        Ok(job)
    }

    pub async fn get_resource_urls(
        &self,
        objects: &[DapObject],
        namespace: &str,
        table: &str,
    ) -> Result<HashMap<String, ResourceEntry>, DapError> {
        if objects.is_empty() {
            return Ok(HashMap::new());
        }
        let resp: ResourceResult = self
            .api_request(
                Method::POST,
                "/dap/object/url",
                Some(objects),
                TableAccess::for_table(namespace, table),
            )
            .await?;
        Ok(resp.urls)
    }

    pub async fn download_objects(
        &self,
        objects: &[DapObject],
        namespace: &str,
        table: &str,
        output_dir: &Path,
        decompress: bool,
    ) -> Result<Vec<PathBuf>, DapError> {
        fs::create_dir_all(output_dir).map_err(|e| {
            DapError::io(format!(
                "Failed to create output directory '{}': {e}",
                output_dir.display()
            ))
        })?;

        if objects.is_empty() {
            return Ok(Vec::new());
        }

        let sem = Arc::new(Semaphore::new(self.download_concurrency));
        let mut handles = Vec::with_capacity(objects.len());

        for obj in objects {
            let client = self.clone();
            let obj = obj.clone();
            let ns = namespace.to_string();
            let tbl = table.to_string();
            let out_dir = output_dir.to_path_buf();
            let sem = sem.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|e| DapError::download(format!("Semaphore closed: {e}")))?;

                let urls = client
                    .get_resource_urls(std::slice::from_ref(&obj), &ns, &tbl)
                    .await?;
                let resource = urls.get(&obj.id).ok_or_else(|| {
                    DapError::download(format!("Pre-signed URL missing for object '{}'", obj.id))
                })?;
                client
                    .download_resource(&resource.url, &out_dir, decompress)
                    .await
            }));
        }

        let mut paths = Vec::with_capacity(handles.len());
        for handle in handles {
            let path = handle
                .await
                .map_err(|e| DapError::download(format!("Download task panicked: {e}")))??;
            paths.push(path);
        }
        paths.sort();
        Ok(paths)
    }

    async fn download_resource(
        &self,
        presigned_url: &str,
        output_dir: &Path,
        decompress: bool,
    ) -> Result<PathBuf, DapError> {
        let parsed = Url::parse(presigned_url)
            .map_err(|e| DapError::download(format!("Invalid pre-signed download URL: {e}")))?;

        let raw_name = parsed
            .path_segments()
            .and_then(|mut s| s.next_back())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                DapError::download("Pre-signed URL does not contain a filename segment")
            })?;

        if raw_name.contains("..") || raw_name.contains('/') || raw_name.contains('\\') {
            return Err(DapError::download(format!(
                "Refusing unsafe filename from server URL: '{raw_name}'"
            )));
        }

        let file_name = if decompress {
            raw_name.strip_suffix(".gz").unwrap_or(raw_name)
        } else {
            raw_name
        };

        let target_path = output_dir.join(file_name);

        let mut attempt = 0usize;
        let resp = loop {
            match self.download_http.get(presigned_url).send().await {
                Ok(r) if r.status().is_success() => break r,
                Ok(r)
                    if (r.status() == StatusCode::TOO_MANY_REQUESTS
                        || r.status().is_server_error())
                        && attempt + 1 < MAX_HTTP_RETRIES =>
                {
                    attempt += 1;
                    tokio::time::sleep(backoff_duration(attempt, None)).await;
                }
                Ok(r) => {
                    return Err(DapError::download(format!(
                        "Pre-signed download failed with HTTP {} for '{}'",
                        r.status().as_u16(),
                        raw_name
                    )));
                }
                Err(_) if attempt + 1 < MAX_HTTP_RETRIES => {
                    attempt += 1;
                    tokio::time::sleep(backoff_duration(attempt, None)).await;
                }
                Err(e) => {
                    return Err(DapError::download(format!(
                        "Pre-signed download network error for '{raw_name}': {e}"
                    )));
                }
            }
        };

        let file = File::create(&target_path).map_err(|e| {
            DapError::io(format!(
                "Failed to create download file '{}': {e}",
                target_path.display()
            ))
        })?;

        let mut stream = resp.bytes_stream();
        if decompress && raw_name.ends_with(".gz") {
            let mut decoder = MultiGzDecoder::new(file);
            while let Some(chunk) = stream.next().await {
                let bytes = chunk.map_err(|e| {
                    DapError::download(format!("Stream error for '{raw_name}': {e}"))
                })?;
                decoder.write_all(&bytes).map_err(|e| {
                    DapError::download(format!("Gzip decompress error for '{raw_name}': {e}"))
                })?;
            }
            decoder.finish().map_err(|e| {
                DapError::download(format!("Gzip finish error for '{raw_name}': {e}"))
            })?;
        } else {
            let mut writer = file;
            while let Some(chunk) = stream.next().await {
                let bytes = chunk.map_err(|e| {
                    DapError::download(format!("Stream error for '{raw_name}': {e}"))
                })?;
                writer.write_all(&bytes).map_err(|e| {
                    DapError::io(format!("Write failed for '{}': {e}", target_path.display()))
                })?;
            }
            writer.flush().map_err(|e| {
                DapError::io(format!("Flush failed for '{}': {e}", target_path.display()))
            })?;
        }

        Ok(target_path)
    }
}

fn verify_granted_table_access(
    namespace: &str,
    table: &str,
    granted: Option<&serde_json::Value>,
) -> Result<(), DapError> {
    if let Some(entries) = granted.and_then(|v| v.as_array()) {
        for entry in entries {
            if entry.get("type").and_then(|v| v.as_str()) != Some("dap") {
                continue;
            }
            if let Some(tables) = entry
                .get("namespaces")
                .and_then(|v| v.as_object())
                .and_then(|ns_map| ns_map.get(namespace))
                .and_then(|v| v.as_array())
                && tables.iter().any(|t| t.as_str() == Some(table))
            {
                return Ok(());
            }
        }
    }
    Err(DapError::table_access_denied(namespace, table))
}

fn decode_jwt_expiry(jwt: &str) -> Option<DateTime<Utc>> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let val: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    let exp = val.get("exp")?.as_i64()?;
    DateTime::from_timestamp(exp, 0)
}

fn backoff_duration(attempt: usize, retry_after_secs: Option<u64>) -> Duration {
    if let Some(secs) = retry_after_secs {
        return Duration::from_secs(secs.min(60));
    }
    let ms = 250u64.saturating_mul(1u64 << attempt.min(6));
    Duration::from_millis(ms.min(10_000))
}

fn map_api_error(status: StatusCode, path: &str, body: &str) -> DapError {
    if let Ok(env) = serde_json::from_str::<DapApiErrorEnvelope>(body) {
        let err_type = env.error.r#type.unwrap_or_default();
        let msg = env
            .error
            .message
            .unwrap_or_else(|| format!("HTTP {} on {}", status.as_u16(), path));
        let uuid = env.error.uuid;

        if err_type.eq_ignore_ascii_case("SnapshotRequired")
            || msg.to_lowercase().contains("snapshot required")
        {
            return DapError::snapshot_required(msg);
        }
        if status == StatusCode::UNAUTHORIZED {
            return DapError::auth(msg, "Check DAP_CLIENT_ID and DAP_CLIENT_SECRET and retry.");
        }

        let detail = serde_json::json!({
            "http_status": status.as_u16(),
            "type": err_type,
            "uuid": uuid,
            "path": path,
        });
        return DapError::api(msg, None, Some(detail));
    }

    DapError::api(
        format!(
            "DAP API {} returned HTTP {}: {}",
            path,
            status.as_u16(),
            truncate(body, 300)
        ),
        None,
        Some(serde_json::json!({ "http_status": status.as_u16(), "path": path })),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max])
    }
}
