mod cli;
mod client;
mod error;
mod output;
mod pg;
mod schema;
mod state;
mod types;
mod unpack;

use crate::cli::{
    Cli, Command, DropDbArgs, IncrementalArgs, InitDbArgs, ListArgs, ListDbArgs, SchemaArgs,
    SkillArgs, SnapshotArgs, SyncDbArgs,
};
use crate::client::DapClient;
use crate::error::DapError;
use crate::output::{ResolvedFormat, print_error, print_structured};
use crate::pg::{
    DropDbTableReport, InitDbTableReport, ListDbEntry, SyncDbTableReport, connect_pg,
    dropdb_tables, ensure_meta_table, get_table_sync_meta, initdb_apply, listdb, syncdb_apply,
};
use crate::schema::{parse_dap_schema, to_datakeel_toml, to_odcs_yaml};
use crate::state::WatermarkStore;
use crate::types::{ContractFormat, DataFormat};
use crate::unpack::unpack_files_to_writer;
use chrono::{DateTime, Utc};
use clap::Parser;
use owo_colors::OwoColorize;
use serde::Serialize;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::PathBuf;

const SKILL_MD: &str = include_str!("../SKILL.md");

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let fmt = cli.format.resolve();

    if let Err(err) = run(cli, fmt).await {
        print_error(fmt, &err);
        std::process::exit(err.exit_code());
    }
}

async fn run(cli: Cli, fmt: ResolvedFormat) -> Result<(), DapError> {
    match &cli.command {
        Command::Skill(args) => handle_skill(args, fmt),
        Command::Dropdb(args) => handle_dropdb(args, fmt).await,
        Command::Listdb(args) => handle_listdb(&cli, args, fmt).await,
        Command::Namespaces => {
            let client = build_client(&cli)?;
            handle_namespaces(&client, fmt).await
        }
        Command::List(args) => {
            let client = build_client(&cli)?;
            handle_list(&client, args, fmt).await
        }
        Command::Schema(args) => {
            let client = build_client(&cli)?;
            handle_schema(&client, args, fmt).await
        }
        Command::Snapshot(args) => {
            let client = build_client(&cli)?;
            handle_snapshot(&client, args, fmt).await
        }
        Command::Incremental(args) => {
            let client = build_client(&cli)?;
            handle_incremental(&client, args, fmt).await
        }
        Command::Initdb(args) => {
            let client = build_client(&cli)?;
            handle_initdb(&client, args, fmt).await
        }
        Command::Syncdb(args) => {
            let client = build_client(&cli)?;
            handle_syncdb(&client, args, fmt).await
        }
    }
}

fn build_client(cli: &Cli) -> Result<DapClient, DapError> {
    DapClient::new(
        cli.api_url.clone().unwrap_or_default(),
        cli.client_id.clone().unwrap_or_default(),
        cli.client_secret.clone().unwrap_or_default(),
        cli.poll_interval_ms,
        cli.concurrency,
        cli.auth_mode,
    )
}

fn resolve_conn_str(opt: &Option<String>) -> Result<String, DapError> {
    if let Some(s) = opt
        && !s.trim().is_empty()
    {
        return Ok(s.clone());
    }
    if let Ok(db_url) = std::env::var("DATABASE_URL")
        && !db_url.trim().is_empty()
    {
        return Ok(db_url);
    }
    Err(DapError::config(
        "Missing PostgreSQL connection string",
        "Set DAP_CONNECTION_STRING or DATABASE_URL, or pass --connection-string.",
    ))
}

fn parse_rfc3339(raw: &str, flag: &str) -> Result<DateTime<Utc>, DapError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            DapError::config(
                format!("Invalid RFC 3339 timestamp for {flag} ('{raw}'): {e}"),
                "Use ISO 8601 / RFC 3339 UTC format such as 2026-11-20T00:00:00Z.",
            )
        })
}

fn handle_skill(args: &SkillArgs, fmt: ResolvedFormat) -> Result<(), DapError> {
    if let Some(dir) = &args.install {
        let target_dir = dir.join("dapkeel");
        fs::create_dir_all(&target_dir).map_err(|e| {
            DapError::io(format!("Failed creating '{}': {e}", target_dir.display()))
        })?;
        let target_file = target_dir.join("SKILL.md");
        fs::write(&target_file, SKILL_MD).map_err(|e| {
            DapError::io(format!("Failed writing '{}': {e}", target_file.display()))
        })?;
        #[derive(Serialize)]
        struct Resp {
            ok: bool,
            installed: String,
        }
        if fmt.is_structured() {
            print_structured(
                fmt,
                &Resp {
                    ok: true,
                    installed: target_file.display().to_string(),
                },
                false,
            );
        } else {
            println!(
                "{} Installed skill to {}",
                "✓".green().bold(),
                target_file.display()
            );
        }
    } else {
        print!("{SKILL_MD}");
    }
    Ok(())
}

async fn handle_namespaces(client: &DapClient, fmt: ResolvedFormat) -> Result<(), DapError> {
    let namespaces = client.list_namespaces().await?;
    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        count: usize,
        namespaces: Vec<String>,
    }
    if fmt.is_structured() {
        print_structured(
            fmt,
            &Resp {
                ok: true,
                count: namespaces.len(),
                namespaces,
            },
            false,
        );
    } else {
        for ns in &namespaces {
            println!("{ns}");
        }
    }
    Ok(())
}

async fn handle_list(
    client: &DapClient,
    args: &ListArgs,
    fmt: ResolvedFormat,
) -> Result<(), DapError> {
    let tables = client.list_tables(&args.namespace).await?;
    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        namespace: String,
        count: usize,
        tables: Vec<String>,
    }
    if fmt.is_structured() {
        print_structured(
            fmt,
            &Resp {
                ok: true,
                namespace: args.namespace.clone(),
                count: tables.len(),
                tables,
            },
            false,
        );
    } else {
        for tbl in &tables {
            println!("{tbl}");
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct SchemaItemReport {
    namespace: String,
    table: String,
    schema_version: i32,
    columns: usize,
    primary_keys: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    contract_file: Option<String>,
}

async fn handle_schema(
    client: &DapClient,
    args: &SchemaArgs,
    fmt: ResolvedFormat,
) -> Result<(), DapError> {
    let tables = client.resolve_tables(&args.namespace, &args.table).await?;
    if args.stdout && tables.len() != 1 {
        return Err(DapError::config(
            "--stdout requires exactly one table",
            "Specify a single --table when using --stdout.",
        ));
    }

    if !args.stdout {
        fs::create_dir_all(&args.output_directory).map_err(|e| {
            DapError::io(format!(
                "Failed to create '{}': {e}",
                args.output_directory.display()
            ))
        })?;
    }

    let mut reports = Vec::with_capacity(tables.len());
    for tbl in tables {
        let vs = client.get_schema(&args.namespace, &tbl).await?;
        let spec = parse_dap_schema(&args.namespace, &tbl, &vs)?;

        if args.stdout {
            match args.to_contract {
                Some(ContractFormat::Odcs) => print!("{}", to_odcs_yaml(&spec)?),
                Some(ContractFormat::Toml) => print!("{}", to_datakeel_toml(&spec)),
                None => println!(
                    "{}",
                    serde_json::to_string_pretty(&vs)
                        .map_err(|e| DapError::schema(format!("JSON serialize failed: {e}")))?
                ),
            }
            return Ok(());
        }

        let schema_path = args
            .output_directory
            .join(format!("{tbl}_schema_version_{}.json", vs.version));
        let json_text = serde_json::to_string_pretty(&vs)
            .map_err(|e| DapError::schema(format!("JSON serialize failed: {e}")))?;
        fs::write(&schema_path, json_text).map_err(|e| {
            DapError::io(format!("Failed writing '{}': {e}", schema_path.display()))
        })?;

        let contract_file = if let Some(cf) = args.to_contract {
            let (path, content) = match cf {
                ContractFormat::Odcs => (
                    args.output_directory
                        .join(format!("{tbl}.datacontract.yaml")),
                    to_odcs_yaml(&spec)?,
                ),
                ContractFormat::Toml => (
                    args.output_directory.join(format!("{tbl}.datakeel.toml")),
                    to_datakeel_toml(&spec),
                ),
            };
            fs::write(&path, content)
                .map_err(|e| DapError::io(format!("Failed writing '{}': {e}", path.display())))?;
            Some(path.display().to_string())
        } else {
            None
        };

        reports.push(SchemaItemReport {
            namespace: args.namespace.clone(),
            table: tbl,
            schema_version: vs.version,
            columns: spec.columns.len(),
            primary_keys: spec.primary_keys,
            schema_file: Some(schema_path.display().to_string()),
            contract_file,
        });
    }

    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        summary: String,
        schemas: Vec<SchemaItemReport>,
    }

    if fmt.is_structured() {
        let summary = format!(
            "Downloaded {} schema(s) from {}",
            reports.len(),
            args.namespace
        );
        print_structured(
            fmt,
            &Resp {
                ok: true,
                summary,
                schemas: reports,
            },
            false,
        );
    } else {
        for r in &reports {
            println!(
                "{} {}.{} (v{}, {} cols) -> {}",
                "✓".green().bold(),
                r.namespace,
                r.table.bold(),
                r.schema_version,
                r.columns,
                r.schema_file.as_deref().unwrap_or("")
            );
            if let Some(cf) = &r.contract_file {
                println!("  {} contract -> {}", "↳".cyan(), cf);
            }
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct ExtractTableReport {
    namespace: String,
    table: String,
    mode: String,
    job_id: String,
    schema_version: i32,
    timestamp: Option<DateTime<Utc>>,
    format: DataFormat,
    files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unpacked_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows_unpacked: Option<u64>,
}

async fn handle_snapshot(
    client: &DapClient,
    args: &SnapshotArgs,
    fmt: ResolvedFormat,
) -> Result<(), DapError> {
    validate_extract_flags(args.data_format, args.decompress, args.unpack, args.stdout)?;
    let tables = client.resolve_tables(&args.namespace, &args.table).await?;
    if args.stdout && tables.len() != 1 {
        return Err(DapError::config(
            "--stdout requires exactly one table",
            "Pass a single table name to --table when streaming to stdout.",
        ));
    }

    let mut store = match &args.state {
        Some(p) => Some((p.clone(), WatermarkStore::load(p)?)),
        None => None,
    };

    let mut reports = Vec::with_capacity(tables.len());
    for tbl in tables {
        let job = client
            .execute_snapshot_job(&args.namespace, &tbl, args.data_format, args.mode)
            .await?;

        let objects = job.objects.clone().unwrap_or_default();
        let job_dir = args.output_directory.join(format!("job_{}", job.id));
        let downloaded = client
            .download_objects(&objects, &args.namespace, &tbl, &job_dir, args.decompress)
            .await?;

        let (unpacked_file, rows_unpacked) = maybe_unpack(
            args.unpack,
            args.stdout,
            !args.no_meta,
            &downloaded,
            &args.output_directory,
            &tbl,
        )?;

        if let (Some((state_path, ws)), Some(ts)) = (store.as_mut(), job.watermark()) {
            ws.set(
                &args.namespace,
                &tbl,
                ts,
                job.schema_version.unwrap_or(1),
                &job.id,
            );
            ws.save_atomic(state_path)?;
        }

        let watermark = job.watermark();
        reports.push(ExtractTableReport {
            namespace: args.namespace.clone(),
            table: tbl,
            mode: "snapshot".to_string(),
            job_id: job.id,
            schema_version: job.schema_version.unwrap_or(1),
            timestamp: watermark,
            format: args.data_format,
            files: downloaded.iter().map(|p| p.display().to_string()).collect(),
            unpacked_file,
            rows_unpacked,
        });
    }

    emit_extract_reports(fmt, reports, args.stdout);
    Ok(())
}

async fn handle_incremental(
    client: &DapClient,
    args: &IncrementalArgs,
    fmt: ResolvedFormat,
) -> Result<(), DapError> {
    validate_extract_flags(args.data_format, args.decompress, args.unpack, args.stdout)?;
    let tables = client.resolve_tables(&args.namespace, &args.table).await?;
    if args.stdout && tables.len() != 1 {
        return Err(DapError::config(
            "--stdout requires exactly one table",
            "Pass a single table name to --table when streaming to stdout.",
        ));
    }

    let explicit_since = match &args.since {
        Some(raw) => Some(parse_rfc3339(raw, "--since")?),
        None => None,
    };
    let until = match &args.until {
        Some(raw) => Some(parse_rfc3339(raw, "--until")?),
        None => None,
    };

    let mut store = match &args.state {
        Some(p) => Some((p.clone(), WatermarkStore::load(p)?)),
        None => None,
    };

    let mut reports = Vec::with_capacity(tables.len());
    for tbl in tables {
        let since = if let Some(ts) = explicit_since {
            ts
        } else if let Some((_, ws)) = &store
            && let Some(wm) = ws.get(&args.namespace, &tbl)
        {
            wm.timestamp
        } else {
            return Err(DapError::config(
                format!(
                    "No --since timestamp provided and no stored watermark for '{}.{}'",
                    args.namespace, tbl
                ),
                "Pass --since <RFC3339> or run `dapkeel snapshot --state <file>` first.",
            ));
        };

        let job = client
            .execute_incremental_job(
                &args.namespace,
                &tbl,
                args.data_format,
                args.mode,
                since,
                until,
            )
            .await?;

        let objects = job.objects.clone().unwrap_or_default();
        let job_dir = args.output_directory.join(format!("job_{}", job.id));
        let downloaded = client
            .download_objects(&objects, &args.namespace, &tbl, &job_dir, args.decompress)
            .await?;

        let (unpacked_file, rows_unpacked) = maybe_unpack(
            args.unpack,
            args.stdout,
            !args.no_meta,
            &downloaded,
            &args.output_directory,
            &tbl,
        )?;

        if let (Some((state_path, ws)), Some(ts)) = (store.as_mut(), job.watermark()) {
            ws.set(
                &args.namespace,
                &tbl,
                ts,
                job.schema_version.unwrap_or(1),
                &job.id,
            );
            ws.save_atomic(state_path)?;
        }

        let watermark = job.watermark();
        reports.push(ExtractTableReport {
            namespace: args.namespace.clone(),
            table: tbl,
            mode: "incremental".to_string(),
            job_id: job.id,
            schema_version: job.schema_version.unwrap_or(1),
            timestamp: watermark,
            format: args.data_format,
            files: downloaded.iter().map(|p| p.display().to_string()).collect(),
            unpacked_file,
            rows_unpacked,
        });
    }

    emit_extract_reports(fmt, reports, args.stdout);
    Ok(())
}

fn validate_extract_flags(
    data_format: DataFormat,
    decompress: bool,
    unpack: bool,
    stdout: bool,
) -> Result<(), DapError> {
    if decompress && data_format == DataFormat::Parquet {
        return Err(DapError::config(
            "Decompression is not supported for 'parquet' format",
            "Omit --decompress when using --data-format parquet.",
        ));
    }
    if unpack && data_format != DataFormat::Jsonl {
        return Err(DapError::config(
            "--unpack requires --data-format jsonl",
            "Use --data-format jsonl (the default) when flattening DAP envelopes with --unpack.",
        ));
    }
    if stdout && !unpack {
        return Err(DapError::config(
            "--stdout requires --unpack",
            "Pass --unpack --stdout to stream clean flattened JSONL to stdout.",
        ));
    }
    Ok(())
}

fn maybe_unpack(
    unpack: bool,
    to_stdout: bool,
    include_meta: bool,
    files: &[PathBuf],
    output_dir: &std::path::Path,
    table: &str,
) -> Result<(Option<String>, Option<u64>), DapError> {
    if !unpack {
        return Ok((None, None));
    }
    if to_stdout {
        let stdout = std::io::stdout();
        let mut handle = BufWriter::new(stdout.lock());
        let rows = unpack_files_to_writer(files, &mut handle, include_meta)?;
        Ok((None, Some(rows)))
    } else {
        let out_path = output_dir.join(format!("{table}.jsonl"));
        let file = File::create(&out_path)
            .map_err(|e| DapError::io(format!("Failed creating '{}': {e}", out_path.display())))?;
        let mut writer = BufWriter::new(file);
        let rows = unpack_files_to_writer(files, &mut writer, include_meta)?;
        Ok((Some(out_path.display().to_string()), Some(rows)))
    }
}

fn emit_extract_reports(
    fmt: ResolvedFormat,
    reports: Vec<ExtractTableReport>,
    stdout_occupied: bool,
) {
    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        summary: String,
        tables: Vec<ExtractTableReport>,
    }
    if fmt.is_structured() {
        let total_files: usize = reports.iter().map(|r| r.files.len()).sum();
        let total_rows: u64 = reports.iter().filter_map(|r| r.rows_unpacked).sum();
        let summary = if total_rows > 0 {
            format!(
                "Extracted {} table(s) ({} files, {} rows unpacked)",
                reports.len(),
                total_files,
                total_rows
            )
        } else {
            format!(
                "Extracted {} table(s) ({} files)",
                reports.len(),
                total_files
            )
        };
        print_structured(
            fmt,
            &Resp {
                ok: true,
                summary,
                tables: reports,
            },
            stdout_occupied,
        );
    } else if !stdout_occupied {
        for r in &reports {
            let ts = r
                .timestamp
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "n/a".to_string());
            println!(
                "{} {} {}.{} (job {}, {} files, as_of={})",
                "✓".green().bold(),
                r.mode,
                r.namespace,
                r.table.bold(),
                r.job_id,
                r.files.len(),
                ts
            );
            if let (Some(path), Some(rows)) = (&r.unpacked_file, r.rows_unpacked) {
                println!("  {} unpacked {} rows -> {}", "↳".cyan(), rows, path);
            }
        }
    }
}

async fn handle_initdb(
    client: &DapClient,
    args: &InitDbArgs,
    fmt: ResolvedFormat,
) -> Result<(), DapError> {
    let conn_str = resolve_conn_str(&args.connection_string)?;
    let tables = client.resolve_tables(&args.namespace, &args.table).await?;
    let target_schema = args
        .target_schema
        .clone()
        .unwrap_or_else(|| args.namespace.clone());

    let mut reports: Vec<InitDbTableReport> = Vec::with_capacity(tables.len());

    for tbl in tables {
        let vs = client.get_schema(&args.namespace, &tbl).await?;
        let spec = parse_dap_schema(&args.namespace, &tbl, &vs)?;
        let schema_json = serde_json::to_string(&vs.schema)
            .map_err(|e| DapError::schema(format!("Failed serializing schema JSON: {e}")))?;

        if args.dry_run {
            let rep = initdb_apply(
                &conn_str,
                &args.meta_schema,
                &target_schema,
                &spec,
                &schema_json,
                Utc::now(),
                &[],
                true,
            )?;
            reports.push(rep);
            continue;
        }

        // Fail fast before running a heavy DAP snapshot job if already initialized
        {
            let cs = conn_str.clone();
            let ms = args.meta_schema.clone();
            let ns = args.namespace.clone();
            let t = tbl.clone();
            let already = tokio::task::spawn_blocking(move || {
                let mut c = connect_pg(&cs)?;
                ensure_meta_table(&mut c, &ms)?;
                Ok::<bool, DapError>(get_table_sync_meta(&mut c, &ms, &ns, &t)?.is_some())
            })
            .await
            .map_err(|e| DapError::db(format!("DB worker panicked: {e}"), None))??;
            if already {
                return Err(DapError::already_initialized(&args.namespace, &tbl));
            }
        }

        let job = client
            .execute_snapshot_job(&args.namespace, &tbl, DataFormat::Jsonl, None)
            .await?;
        let watermark = job.watermark().unwrap_or_else(Utc::now);
        let objects = job.objects.clone().unwrap_or_default();

        let tmp_dir =
            std::env::temp_dir().join(format!("dapkeel-initdb-{}-{}", std::process::id(), job.id));
        let downloaded = client
            .download_objects(&objects, &args.namespace, &tbl, &tmp_dir, false)
            .await?;

        let cs = conn_str.clone();
        let ms = args.meta_schema.clone();
        let ts_schema = target_schema.clone();
        let rep = tokio::task::spawn_blocking(move || {
            initdb_apply(
                &cs,
                &ms,
                &ts_schema,
                &spec,
                &schema_json,
                watermark,
                &downloaded,
                false,
            )
        })
        .await
        .map_err(|e| DapError::db(format!("DB worker panicked: {e}"), None))??;

        let _ = fs::remove_dir_all(&tmp_dir);
        reports.push(rep);
    }

    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        summary: String,
        tables: Vec<InitDbTableReport>,
    }

    if fmt.is_structured() {
        let total_rows: u64 = reports.iter().map(|r| r.rows_loaded).sum();
        let summary = format!(
            "Initialized {} table(s) ({} rows loaded)",
            reports.len(),
            total_rows
        );
        print_structured(
            fmt,
            &Resp {
                ok: true,
                summary,
                tables: reports,
            },
            false,
        );
    } else {
        for r in &reports {
            if r.dry_run {
                println!(
                    "{} [dry-run] initdb {}.{} -> {} (v{})",
                    "•".cyan().bold(),
                    r.namespace,
                    r.table.bold(),
                    r.target,
                    r.schema_version
                );
                if let Some(ddl) = &r.ddl {
                    println!("{ddl}");
                }
            } else {
                println!(
                    "{} initdb {}.{} -> {} ({} rows loaded, {} deduped, as_of={})",
                    "✓".green().bold(),
                    r.namespace,
                    r.table.bold(),
                    r.target,
                    r.rows_loaded,
                    r.duplicates_deduped,
                    r.timestamp.to_rfc3339()
                );
            }
        }
    }
    Ok(())
}

async fn handle_syncdb(
    client: &DapClient,
    args: &SyncDbArgs,
    fmt: ResolvedFormat,
) -> Result<(), DapError> {
    let conn_str = resolve_conn_str(&args.connection_string)?;
    let tables = if args.table.trim().eq_ignore_ascii_case("all") {
        let cs = conn_str.clone();
        let ms = args.meta_schema.clone();
        let ns = args.namespace.clone();
        let entries = tokio::task::spawn_blocking(move || listdb(&cs, &ms, &ns, None, true, true))
            .await
            .map_err(|e| DapError::db(format!("DB worker panicked: {e}"), None))??;
        entries.into_iter().map(|e| e.name).collect()
    } else {
        client.resolve_tables(&args.namespace, &args.table).await?
    };

    let mut reports: Vec<SyncDbTableReport> = Vec::with_capacity(tables.len());

    for tbl in tables {
        let meta = {
            let cs = conn_str.clone();
            let ms = args.meta_schema.clone();
            let ns = args.namespace.clone();
            let t = tbl.clone();
            tokio::task::spawn_blocking(move || {
                let mut c = connect_pg(&cs)?;
                ensure_meta_table(&mut c, &ms)?;
                get_table_sync_meta(&mut c, &ms, &ns, &t)
            })
            .await
            .map_err(|e| DapError::db(format!("DB worker panicked: {e}"), None))??
        };

        let Some(meta) = meta else {
            return Err(DapError::not_initialized(&args.namespace, &tbl));
        };

        let vs = client.get_schema(&args.namespace, &tbl).await?;
        let spec = parse_dap_schema(&args.namespace, &tbl, &vs)?;
        let schema_json = serde_json::to_string(&vs.schema)
            .map_err(|e| DapError::schema(format!("Failed serializing schema JSON: {e}")))?;

        let job = client
            .execute_incremental_job(
                &args.namespace,
                &tbl,
                DataFormat::Jsonl,
                None,
                meta.timestamp,
                None,
            )
            .await?;
        let new_watermark = job.watermark().unwrap_or(meta.timestamp);
        let objects = job.objects.clone().unwrap_or_default();

        let tmp_dir =
            std::env::temp_dir().join(format!("dapkeel-syncdb-{}-{}", std::process::id(), job.id));
        let downloaded = client
            .download_objects(&objects, &args.namespace, &tbl, &tmp_dir, false)
            .await?;

        let cs = conn_str.clone();
        let ms = args.meta_schema.clone();
        let soft_del = args.soft_delete;
        let dry = args.dry_run;
        let rep = tokio::task::spawn_blocking(move || {
            syncdb_apply(
                &cs,
                &ms,
                &meta,
                &spec,
                &schema_json,
                new_watermark,
                &downloaded,
                soft_del,
                dry,
            )
        })
        .await
        .map_err(|e| DapError::db(format!("DB worker panicked: {e}"), None))??;

        let _ = fs::remove_dir_all(&tmp_dir);
        reports.push(rep);
    }

    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        summary: String,
        tables: Vec<SyncDbTableReport>,
    }

    if fmt.is_structured() {
        let upserted: u64 = reports.iter().map(|r| r.rows_upserted).sum();
        let deleted: u64 = reports.iter().map(|r| r.rows_deleted).sum();
        let summary = format!(
            "Synced {} table(s) ({} upserted, {} deleted)",
            reports.len(),
            upserted,
            deleted
        );
        print_structured(
            fmt,
            &Resp {
                ok: true,
                summary,
                tables: reports,
            },
            false,
        );
    } else {
        for r in &reports {
            let prefix = if r.dry_run {
                "[dry-run] syncdb"
            } else {
                "syncdb"
            };
            println!(
                "{} {} {}.{} -> {} ({} upserted, {} deleted, {} cols added, as_of={})",
                "✓".green().bold(),
                prefix,
                r.namespace,
                r.table.bold(),
                r.target,
                r.rows_upserted,
                r.rows_deleted,
                r.columns_added.len(),
                r.new_timestamp.to_rfc3339()
            );
        }
    }
    Ok(())
}

async fn handle_listdb(cli: &Cli, args: &ListDbArgs, fmt: ResolvedFormat) -> Result<(), DapError> {
    let conn_str = resolve_conn_str(&args.connection_string)?;

    let remote_tables = if !args.offline && !args.omit_not_replicated {
        match build_client(cli) {
            Ok(client) => client.list_tables(&args.namespace).await.ok(),
            Err(_) => None,
        }
    } else {
        None
    };

    let ms = args.meta_schema.clone();
    let ns = args.namespace.clone();
    let omit_rc = args.omit_record_count;
    let omit_nr = args.omit_not_replicated;

    let entries: Vec<ListDbEntry> = tokio::task::spawn_blocking(move || {
        listdb(
            &conn_str,
            &ms,
            &ns,
            remote_tables.as_deref(),
            omit_rc,
            omit_nr,
        )
    })
    .await
    .map_err(|e| DapError::db(format!("DB worker panicked: {e}"), None))??;

    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        namespace: String,
        tables: Vec<ListDbEntry>,
    }

    if fmt.is_structured() {
        print_structured(
            fmt,
            &Resp {
                ok: true,
                namespace: args.namespace.clone(),
                tables: entries,
            },
            false,
        );
    } else {
        println!(
            "{:<28} {:>10} {:<26} {:<12} {:>6}  {}",
            "TABLE".bold(),
            "RECORDS".bold(),
            "DATA AS OF".bold(),
            "STALENESS".bold(),
            "SCHEMA".bold(),
            "NEXT".bold()
        );
        for e in &entries {
            let rec = e
                .records
                .map(|n| n.to_string())
                .unwrap_or_else(|| "-".to_string());
            let ts = e
                .timestamp
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "Not replicated".to_string());
            let stale = e.staleness.as_deref().unwrap_or("-");
            let sv = e
                .schema_version
                .map(|v| format!("v{v}"))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "{:<28} {:>10} {:<26} {:<12} {:>6}  {}",
                e.name, rec, ts, stale, sv, e.command
            );
        }
    }
    Ok(())
}

async fn handle_dropdb(args: &DropDbArgs, fmt: ResolvedFormat) -> Result<(), DapError> {
    let conn_str = resolve_conn_str(&args.connection_string)?;
    let ms = args.meta_schema.clone();
    let ns = args.namespace.clone();
    let tbl = args.table.clone();
    let reports: Vec<DropDbTableReport> =
        tokio::task::spawn_blocking(move || dropdb_tables(&conn_str, &ms, &ns, &tbl))
            .await
            .map_err(|e| DapError::db(format!("DB worker panicked: {e}"), None))??;

    #[derive(Serialize)]
    struct Resp {
        ok: bool,
        tables: Vec<DropDbTableReport>,
    }

    if fmt.is_structured() {
        print_structured(
            fmt,
            &Resp {
                ok: true,
                tables: reports,
            },
            false,
        );
    } else {
        for r in &reports {
            if r.dropped {
                println!("{} dropped {}", "✓".green().bold(), r.target.bold());
            } else {
                println!(
                    "{} {} not found in table_sync (skipped)",
                    "•".yellow(),
                    r.target
                );
            }
        }
    }
    Ok(())
}
