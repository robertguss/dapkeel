use assert_cmd::Command;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use std::io::Write;
use tempfile::TempDir;
use wiremock::matchers::{header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

fn dapkeel() -> Command {
    Command::cargo_bin("dapkeel").expect("dapkeel binary")
}

fn gzip_bytes(input: &str) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(input.as_bytes()).expect("gzip write");
    enc.finish().expect("gzip finish")
}

fn fake_jwt(exp_unix: i64) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let h = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","typ":"JWT"}"#);
    let p = URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp_unix},"sub":"dap-test"}}"#));
    format!("{h}.{p}.sig")
}

async fn mount_oauth(server: &MockServer, granted_table: Option<(&str, &str)>) {
    let jwt = fake_jwt(chrono::Utc::now().timestamp() + 3600);
    let auth_details = granted_table.map(|(ns, tbl)| {
        json!([{
            "type": "dap",
            "namespaces": { ns: [tbl] }
        }])
    });

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .and(header_exists("authorization"))
        .respond_with(move |_req: &Request| {
            let mut body = json!({
                "access_token": jwt,
                "expires_in": 3600,
                "scope": "dap",
                "token_type": "Bearer"
            });
            if let Some(ad) = &auth_details {
                body["authorization_details"] = ad.clone();
            }
            ResponseTemplate::new(200).set_body_json(body)
        })
        .mount(server)
        .await;
}

#[tokio::test]
async fn namespaces_list_and_schema_contract_export() {
    let server = MockServer::start().await;
    mount_oauth(&server, None).await;

    Mock::given(method("GET"))
        .and(path("/dap/query/namespace"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "namespaces": ["canvas", "canvas_logs", "catalog"]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/dap/query/canvas/table"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tables": ["courses", "enrollments", "users"]
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/dap/query/canvas/table/users/schema"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": 2,
            "schema": {
                "type": "object",
                "title": "Canvas LMS users",
                "properties": {
                    "key": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "integer", "format": "int64" }
                        },
                        "required": ["id"]
                    },
                    "value": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "workflow_state": { "type": "string", "enum": ["registered", "deleted"] },
                            "created_at": { "type": "string", "format": "date-time" }
                        },
                        "required": ["workflow_state"]
                    },
                    "meta": {
                        "type": "object",
                        "properties": {
                            "ts": { "type": "string", "format": "date-time" },
                            "action": { "type": "string", "enum": ["U", "D"] }
                        }
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    // 1. namespaces
    let out = dapkeel()
        .args([
            "--api-url",
            &server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--format",
            "agent",
            "namespaces",
        ])
        .output()
        .expect("run namespaces");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["ok"], true);
    assert_eq!(val["count"], 3);

    // 2. list
    let out = dapkeel()
        .args([
            "--api-url",
            &server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--format",
            "agent",
            "list",
            "--namespace",
            "canvas",
        ])
        .output()
        .expect("run list");
    assert!(out.status.success());
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["tables"], json!(["courses", "enrollments", "users"]));

    // 3. schema + ODCS v3.2.0 export
    let dir = TempDir::new().expect("tmpdir");
    let out = dapkeel()
        .args([
            "--api-url",
            &server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--format",
            "agent",
            "schema",
            "--namespace",
            "canvas",
            "--table",
            "users",
            "--output-directory",
            dir.path().to_str().unwrap(),
            "--to-contract",
            "odcs",
        ])
        .output()
        .expect("run schema");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir.path().join("users_schema_version_2.json").exists());
    let odcs = std::fs::read_to_string(dir.path().join("users.datacontract.yaml")).unwrap();
    assert!(odcs.contains("apiVersion: v3.2.0"));
    assert!(odcs.contains("primaryKey: true"));
    assert!(odcs.contains("registered"));
}

#[tokio::test]
async fn snapshot_then_incremental_with_unpack_state_and_channel_isolation() {
    let api_server = MockServer::start().await;
    let s3_server = MockServer::start().await;
    mount_oauth(&api_server, Some(("canvas", "users"))).await;

    // Snapshot job: first POST returns running, GET returns complete
    Mock::given(method("POST"))
        .and(path("/dap/query/canvas/table/users/data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "job-snap-1",
            "status": "running"
        })))
        .up_to_n_times(1)
        .mount(&api_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/dap/job/job-snap-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "job-snap-1",
            "status": "complete",
            "schema_version": 1,
            "at": "2026-11-19T00:00:00Z",
            "expires_at": "2026-11-25T00:00:00Z",
            "objects": [{ "id": "obj-1" }]
        })))
        .mount(&api_server)
        .await;

    let s3_snap_url = format!("{}/bucket/part-000.jsonl.gz", s3_server.uri());
    Mock::given(method("POST"))
        .and(path("/dap/object/url"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "urls": {
                "obj-1": { "url": s3_snap_url }
            }
        })))
        .up_to_n_times(1)
        .mount(&api_server)
        .await;

    // S3 download server verifies NO Authorization header was leaked!
    let snap_gz = gzip_bytes(
        "{\"schema_version\":1,\"key\":{\"id\":1},\"value\":{\"name\":\"Ada\",\"workflow_state\":\"registered\"},\"meta\":{\"ts\":\"2026-11-19T00:00:00Z\"}}\n\
         {\"schema_version\":1,\"key\":{\"id\":2},\"value\":{\"name\":\"Grace\",\"workflow_state\":\"registered\"},\"meta\":{\"ts\":\"2026-11-19T00:00:00Z\"}}\n",
    );
    Mock::given(method("GET"))
        .and(path("/bucket/part-000.jsonl.gz"))
        .respond_with(move |req: &Request| {
            assert!(
                !req.headers.contains_key("authorization"),
                "Bearer token must NEVER be sent to pre-signed download URLs!"
            );
            ResponseTemplate::new(200).set_body_bytes(snap_gz.clone())
        })
        .mount(&s3_server)
        .await;

    let dir = TempDir::new().expect("tmpdir");
    let state_path = dir.path().join("dap_state.json");

    let out = dapkeel()
        .args([
            "--api-url",
            &api_server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--poll-interval-ms",
            "20",
            "--format",
            "agent",
            "snapshot",
            "--namespace",
            "canvas",
            "--table",
            "users",
            "--output-directory",
            dir.path().to_str().unwrap(),
            "--unpack",
            "--state",
            state_path.to_str().unwrap(),
        ])
        .output()
        .expect("run snapshot");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["ok"], true);
    assert_eq!(val["tables"][0]["rows_unpacked"], 2);
    assert!(state_path.exists());

    let unpacked = std::fs::read_to_string(dir.path().join("users.jsonl")).unwrap();
    assert!(unpacked.contains("\"id\":1"));
    assert!(unpacked.contains("\"name\":\"Grace\""));

    // Now test incremental auto-resuming from state_path!
    Mock::given(method("POST"))
        .and(path("/dap/query/canvas/table/users/data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "job-inc-2",
            "status": "complete",
            "schema_version": 1,
            "since": "2026-11-19T00:00:00Z",
            "until": "2026-11-20T00:00:00Z",
            "expires_at": "2026-11-25T00:00:00Z",
            "objects": [{ "id": "obj-2" }]
        })))
        .mount(&api_server)
        .await;

    let s3_inc_url = format!("{}/bucket/inc-000.jsonl.gz", s3_server.uri());
    Mock::given(method("POST"))
        .and(path("/dap/object/url"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "urls": {
                "obj-2": { "url": s3_inc_url }
            }
        })))
        .mount(&api_server)
        .await;

    let inc_gz = gzip_bytes(
        "{\"schema_version\":1,\"key\":{\"id\":2},\"value\":{\"name\":\"Grace Hopper\",\"workflow_state\":\"registered\"},\"meta\":{\"ts\":\"2026-11-20T00:00:00Z\",\"action\":\"U\"}}\n",
    );
    Mock::given(method("GET"))
        .and(path("/bucket/inc-000.jsonl.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(inc_gz))
        .mount(&s3_server)
        .await;

    let out = dapkeel()
        .args([
            "--api-url",
            &api_server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--poll-interval-ms",
            "20",
            "--format",
            "agent",
            "incremental",
            "--namespace",
            "canvas",
            "--table",
            "users",
            "--state",
            state_path.to_str().unwrap(),
            "--unpack",
            "--stdout",
        ])
        .output()
        .expect("run incremental");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout_str = String::from_utf8_lossy(&out.stdout);
    assert!(stdout_str.contains("\"name\":\"Grace Hopper\""));
    assert!(stdout_str.contains("\"_dap_action\":\"U\""));
}

#[tokio::test]
async fn rejects_missing_table_access_grant() {
    let server = MockServer::start().await;
    // Token issuer only grants canvas/courses, NOT canvas/users!
    mount_oauth(&server, Some(("canvas", "courses"))).await;

    let out = dapkeel()
        .args([
            "--api-url",
            &server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--format",
            "agent",
            "snapshot",
            "--namespace",
            "canvas",
            "--table",
            "users",
        ])
        .output()
        .expect("run snapshot");
    assert!(!out.status.success());
    let val: Value = serde_json::from_slice(&out.stderr).expect("error json");
    assert_eq!(val["ok"], false);
    assert_eq!(val["error"]["code"], "DAP_TABLE_ACCESS_DENIED");
}

#[tokio::test]
async fn auto_falls_back_from_oauth_v3_to_ids_v2() {
    let server = MockServer::start().await;
    let jwt = fake_jwt(chrono::Utc::now().timestamp() + 3600);

    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "Client authentication failed: client_secret"
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/ids/auth/login"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": jwt,
            "expires_in": 3600,
            "scope": "dap",
            "token_type": "Bearer"
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/dap/query/canvas/table"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tables": ["courses", "users"]
        })))
        .mount(&server)
        .await;

    let out = dapkeel()
        .args([
            "--api-url",
            &server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--format",
            "agent",
            "list",
            "--namespace",
            "canvas",
        ])
        .output()
        .expect("run list with fallback");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["ok"], true);
    assert_eq!(val["tables"], json!(["courses", "users"]));
}

#[test]
fn skill_command_prints_and_installs() {
    let out = dapkeel().args(["skill"]).output().expect("skill");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("name: dapkeel"));

    let dir = TempDir::new().expect("tmpdir");
    let out = dapkeel()
        .args([
            "--format",
            "agent",
            "skill",
            "--install",
            dir.path().to_str().unwrap(),
        ])
        .output()
        .expect("skill install");
    assert!(out.status.success());
    assert!(dir.path().join("dapkeel/SKILL.md").exists());
}

/// Live Postgres E2E test: initdb -> listdb -> syncdb (upsert + delete + schema evolution) -> dropdb.
/// Run with: DAPKEEL_TEST_DATABASE_URL="postgres://postgres:postgres@127.0.0.1:5432/postgres?sslmode=disable" cargo test -- --ignored
#[tokio::test]
#[ignore]
async fn postgres_initdb_syncdb_listdb_dropdb_e2e() {
    let db_url = std::env::var("DAPKEEL_TEST_DATABASE_URL")
        .expect("Set DAPKEEL_TEST_DATABASE_URL to run live Postgres E2E test");

    let api_server = MockServer::start().await;
    let s3_server = MockServer::start().await;
    mount_oauth(&api_server, Some(("canvas", "students"))).await;

    // Schema v1 for initdb
    Mock::given(method("GET"))
        .and(path("/dap/query/canvas/table/students/schema"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": 1,
            "schema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "object",
                        "properties": { "id": { "type": "integer", "format": "int64" } },
                        "required": ["id"]
                    },
                    "value": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "gpa": { "type": "number" },
                            "active": { "type": "boolean" }
                        }
                    }
                }
            }
        })))
        .up_to_n_times(1)
        .mount(&api_server)
        .await;

    // Snapshot job + S3 file (includes a duplicate PK to verify last-wins dedupe!)
    Mock::given(method("POST"))
        .and(path("/dap/query/canvas/table/students/data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pg-snap-1",
            "status": "complete",
            "schema_version": 1,
            "at": "2026-11-19T00:00:00Z",
            "expires_at": "2026-11-25T00:00:00Z",
            "objects": [{ "id": "pg-obj-1" }]
        })))
        .up_to_n_times(1)
        .mount(&api_server)
        .await;

    let snap_url = format!("{}/s3/students-snap.jsonl.gz", s3_server.uri());
    Mock::given(method("POST"))
        .and(path("/dap/object/url"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "urls": { "pg-obj-1": { "url": snap_url } }
        })))
        .up_to_n_times(1)
        .mount(&api_server)
        .await;

    let snap_gz = gzip_bytes(
        "{\"schema_version\":1,\"key\":{\"id\":1},\"value\":{\"name\":\"Ada\",\"gpa\":3.9,\"active\":true},\"meta\":{\"ts\":\"2026-11-19T00:00:00Z\"}}\n\
         {\"schema_version\":1,\"key\":{\"id\":2},\"value\":{\"name\":\"Bob\",\"gpa\":2.1,\"active\":true},\"meta\":{\"ts\":\"2026-11-19T00:00:00Z\"}}\n\
         {\"schema_version\":1,\"key\":{\"id\":2},\"value\":{\"name\":\"Bob Updated\",\"gpa\":2.4,\"active\":true},\"meta\":{\"ts\":\"2026-11-19T00:00:00Z\"}}\n\
         {\"schema_version\":1,\"key\":{\"id\":3},\"value\":{\"name\":\"Carol\",\"gpa\":3.5,\"active\":false},\"meta\":{\"ts\":\"2026-11-19T00:00:00Z\"}}\n",
    );
    Mock::given(method("GET"))
        .and(path("/s3/students-snap.jsonl.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(snap_gz))
        .mount(&s3_server)
        .await;

    // 1. initdb
    let out = dapkeel()
        .args([
            "--api-url",
            &api_server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--format",
            "agent",
            "initdb",
            "--namespace",
            "canvas",
            "--table",
            "students",
            "--connection-string",
            &db_url,
        ])
        .output()
        .expect("initdb");
    assert!(
        out.status.success(),
        "initdb stderr: {} stdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["tables"][0]["rows_loaded"], 3);
    assert_eq!(val["tables"][0]["duplicates_deduped"], 1);

    // 2. listdb (--offline)
    let out = dapkeel()
        .args([
            "--format",
            "agent",
            "listdb",
            "--namespace",
            "canvas",
            "--connection-string",
            &db_url,
            "--offline",
        ])
        .output()
        .expect("listdb");
    assert!(out.status.success());
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["tables"][0]["name"], "students");
    assert_eq!(val["tables"][0]["records"], 3);
    assert_eq!(val["tables"][0]["command"], "syncdb");

    // 3. Schema v2 (adds 'advisor' column) + Incremental CDC (upsert id=1, insert id=4, delete id=3)
    Mock::given(method("GET"))
        .and(path("/dap/query/canvas/table/students/schema"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "version": 2,
            "schema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "object",
                        "properties": { "id": { "type": "integer", "format": "int64" } },
                        "required": ["id"]
                    },
                    "value": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "gpa": { "type": "number" },
                            "active": { "type": "boolean" },
                            "advisor": { "type": "string" }
                        }
                    }
                }
            }
        })))
        .mount(&api_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/dap/query/canvas/table/students/data"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "pg-inc-2",
            "status": "complete",
            "schema_version": 2,
            "since": "2026-11-19T00:00:00Z",
            "until": "2026-11-20T00:00:00Z",
            "expires_at": "2026-11-25T00:00:00Z",
            "objects": [{ "id": "pg-obj-2" }]
        })))
        .mount(&api_server)
        .await;

    let inc_url = format!("{}/s3/students-inc.jsonl.gz", s3_server.uri());
    Mock::given(method("POST"))
        .and(path("/dap/object/url"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "urls": { "pg-obj-2": { "url": inc_url } }
        })))
        .mount(&api_server)
        .await;

    let inc_gz = gzip_bytes(
        "{\"schema_version\":2,\"key\":{\"id\":1},\"value\":{\"name\":\"Ada Lovelace\",\"gpa\":4.0,\"active\":true,\"advisor\":\"Dr. Smith\"},\"meta\":{\"ts\":\"2026-11-20T00:00:00Z\",\"action\":\"U\"}}\n\
         {\"schema_version\":2,\"key\":{\"id\":4},\"value\":{\"name\":\"David\",\"gpa\":3.1,\"active\":true,\"advisor\":\"Dr. Jones\"},\"meta\":{\"ts\":\"2026-11-20T00:00:00Z\",\"action\":\"U\"}}\n\
         {\"schema_version\":2,\"key\":{\"id\":3},\"value\":{},\"meta\":{\"ts\":\"2026-11-20T00:00:00Z\",\"action\":\"D\"}}\n",
    );
    Mock::given(method("GET"))
        .and(path("/s3/students-inc.jsonl.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(inc_gz))
        .mount(&s3_server)
        .await;

    let out = dapkeel()
        .args([
            "--api-url",
            &api_server.uri(),
            "--client-id",
            "cid",
            "--client-secret",
            "csec",
            "--format",
            "agent",
            "syncdb",
            "--namespace",
            "canvas",
            "--table",
            "students",
            "--connection-string",
            &db_url,
        ])
        .output()
        .expect("syncdb");
    assert!(
        out.status.success(),
        "syncdb stderr: {} stdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["tables"][0]["rows_upserted"], 2);
    assert_eq!(val["tables"][0]["rows_deleted"], 1);
    assert_eq!(val["tables"][0]["columns_added"], json!(["advisor"]));

    // 4. Verify listdb now reports 3 records (1, 2, 4) and schema_version 2
    let out = dapkeel()
        .args([
            "--format",
            "agent",
            "listdb",
            "--namespace",
            "canvas",
            "--connection-string",
            &db_url,
            "--offline",
        ])
        .output()
        .expect("listdb 2");
    assert!(out.status.success());
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["tables"][0]["records"], 3);
    assert_eq!(val["tables"][0]["schema_version"], 2);

    // 5. dropdb
    let out = dapkeel()
        .args([
            "--format",
            "agent",
            "dropdb",
            "--namespace",
            "canvas",
            "--table",
            "students",
            "--connection-string",
            &db_url,
        ])
        .output()
        .expect("dropdb");
    assert!(out.status.success());
    let val: Value = serde_json::from_slice(&out.stdout).expect("json");
    assert_eq!(val["tables"][0]["dropped"], true);
}
