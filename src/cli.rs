use crate::output::OutputFormat;
use crate::pg::DEFAULT_META_SCHEMA;
use crate::types::{ContractFormat, DataFormat, DataMode};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "dapkeel",
    version,
    about = "Blazing-fast, agent-first (DX + AX) Rust CLI for Instructure Data Access Platform (Canvas Data 2)"
)]
pub struct Cli {
    /// Output format (auto switches to compact JSON when stdout is not a TTY)
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Auto)]
    pub format: OutputFormat,

    /// Instructure regional API Gateway base URL (env: DAP_API_URL)
    #[arg(long, global = true, env = "DAP_API_URL")]
    pub api_url: Option<String>,

    /// Instructure Identity Service OAuth Client ID (env: DAP_CLIENT_ID)
    #[arg(long, global = true, env = "DAP_CLIENT_ID")]
    pub client_id: Option<String>,

    /// Instructure Identity Service OAuth Client Secret (env: DAP_CLIENT_SECRET)
    #[arg(long, global = true, env = "DAP_CLIENT_SECRET")]
    pub client_secret: Option<String>,

    /// Job polling interval in milliseconds
    #[arg(long, global = true, default_value_t = 3000)]
    pub poll_interval_ms: u64,

    /// Concurrent pre-signed object downloads
    #[arg(long, global = true, default_value_t = 4)]
    pub concurrency: usize,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List available DAP namespaces (e.g. canvas, canvas_logs, catalog, new_quizzes)
    Namespaces,

    /// List available tables in a DAP namespace
    List(ListArgs),

    /// Download table JSON schema(s) and optionally export ODCS v3.2.0 or datakeel.toml contracts
    Schema(SchemaArgs),

    /// Download a full point-in-time snapshot of one or more tables
    Snapshot(SnapshotArgs),

    /// Download incremental CDC updates since a timestamp or watermark state file
    Incremental(IncrementalArgs),

    /// Initialize PostgreSQL table(s) from a full DAP snapshot via atomic COPY + table_sync metadata
    Initdb(InitDbArgs),

    /// Synchronize PostgreSQL table(s) with incremental DAP CDC upserts/deletes and schema evolution
    Syncdb(SyncDbArgs),

    /// Inspect local PostgreSQL replication status for a DAP namespace
    Listdb(ListDbArgs),

    /// Drop replicated PostgreSQL table(s) and remove their table_sync metadata
    Dropdb(DropDbArgs),

    /// Print or install the bundled SKILL.md for AI coding agents
    Skill(SkillArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// DAP namespace (e.g. canvas, canvas_logs, catalog, new_quizzes)
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    /// DAP namespace
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,

    /// Single table, comma-separated list, or 'all'
    #[arg(short, long)]
    pub table: String,

    /// Output directory for downloaded schemas / contracts
    #[arg(short, long, default_value = "downloads")]
    pub output_directory: PathBuf,

    /// Also export each DAP schema as a Keel contract (odcs = ODCS v3.2.0 YAML, toml = datakeel.toml)
    #[arg(long, value_enum)]
    pub to_contract: Option<ContractFormat>,

    /// Print single-table schema (or converted contract) to stdout instead of writing files
    #[arg(long)]
    pub stdout: bool,
}

#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// DAP namespace
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,

    /// Single table, comma-separated list, or 'all'
    #[arg(short, long)]
    pub table: String,

    /// DAP export file format
    #[arg(long = "data-format", short = 'd', value_enum, default_value_t = DataFormat::Jsonl)]
    pub data_format: DataFormat,

    /// Nested field output mode (expanded or condensed)
    #[arg(long, value_enum)]
    pub mode: Option<DataMode>,

    /// Destination directory for downloaded part files (and unpacked output)
    #[arg(short, long, default_value = "downloads")]
    pub output_directory: PathBuf,

    /// Decompress downloaded .gz part files on disk
    #[arg(long)]
    pub decompress: bool,

    /// Flatten DAP JSONL envelope ({key, value, meta}) into clean rows for datakeel/loadkeel
    #[arg(long)]
    pub unpack: bool,

    /// Omit _dap_ts / _dap_action / _schema_version when using --unpack
    #[arg(long)]
    pub no_meta: bool,

    /// Stream unpacked JSONL records directly to stdout (requires --unpack and a single table)
    #[arg(long)]
    pub stdout: bool,

    /// Persist watermark timestamp per table in a local JSON state file
    #[arg(long)]
    pub state: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct IncrementalArgs {
    /// DAP namespace
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,

    /// Single table, comma-separated list, or 'all'
    #[arg(short, long)]
    pub table: String,

    /// Start timestamp in RFC 3339 UTC format (optional if --state already has a watermark)
    #[arg(long)]
    pub since: Option<String>,

    /// Optional end timestamp in RFC 3339 UTC format
    #[arg(long)]
    pub until: Option<String>,

    /// DAP export file format
    #[arg(long = "data-format", short = 'd', value_enum, default_value_t = DataFormat::Jsonl)]
    pub data_format: DataFormat,

    /// Nested field output mode
    #[arg(long, value_enum)]
    pub mode: Option<DataMode>,

    /// Destination directory for downloaded part files (and unpacked output)
    #[arg(short, long, default_value = "downloads")]
    pub output_directory: PathBuf,

    /// Decompress downloaded .gz part files on disk
    #[arg(long)]
    pub decompress: bool,

    /// Flatten DAP JSONL envelope ({key, value, meta}) into clean rows for datakeel/loadkeel
    #[arg(long)]
    pub unpack: bool,

    /// Omit _dap_ts / _dap_action / _schema_version when using --unpack
    #[arg(long)]
    pub no_meta: bool,

    /// Stream unpacked JSONL records directly to stdout (requires --unpack and a single table)
    #[arg(long)]
    pub stdout: bool,

    /// Read and atomically advance watermarks in a local JSON state file
    #[arg(long)]
    pub state: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct InitDbArgs {
    /// DAP namespace
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,

    /// Single table, comma-separated list, or 'all'
    #[arg(short, long)]
    pub table: String,

    /// PostgreSQL connection string (env: DAP_CONNECTION_STRING or DATABASE_URL)
    #[arg(long, env = "DAP_CONNECTION_STRING")]
    pub connection_string: Option<String>,

    /// Target PostgreSQL schema (defaults to the DAP namespace)
    #[arg(long)]
    pub target_schema: Option<String>,

    /// Metadata schema storing table_sync (default: instructure_dap)
    #[arg(long, default_value = DEFAULT_META_SCHEMA)]
    pub meta_schema: String,

    /// Print planned DDL and actions without mutating PostgreSQL
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct SyncDbArgs {
    /// DAP namespace
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,

    /// Single table, comma-separated list, or 'all'
    #[arg(short, long)]
    pub table: String,

    /// PostgreSQL connection string (env: DAP_CONNECTION_STRING or DATABASE_URL)
    #[arg(long, env = "DAP_CONNECTION_STRING")]
    pub connection_string: Option<String>,

    /// Metadata schema storing table_sync (default: instructure_dap)
    #[arg(long, default_value = DEFAULT_META_SCHEMA)]
    pub meta_schema: String,

    /// Mark CDC 'D' deletes with _dap_deleted = TRUE instead of hard DELETE
    #[arg(long)]
    pub soft_delete: bool,

    /// Fetch incremental changes and report counts without mutating PostgreSQL
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ListDbArgs {
    /// DAP namespace
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,

    /// PostgreSQL connection string (env: DAP_CONNECTION_STRING or DATABASE_URL)
    #[arg(long, env = "DAP_CONNECTION_STRING")]
    pub connection_string: Option<String>,

    /// Metadata schema storing table_sync (default: instructure_dap)
    #[arg(long, default_value = DEFAULT_META_SCHEMA)]
    pub meta_schema: String,

    /// Skip SELECT COUNT(*) queries on replicated tables
    #[arg(long)]
    pub omit_record_count: bool,

    /// Only list tables currently present in table_sync
    #[arg(long)]
    pub omit_not_replicated: bool,

    /// Inspect local Postgres only without calling the remote DAP API
    #[arg(long)]
    pub offline: bool,
}

#[derive(Debug, Args)]
pub struct DropDbArgs {
    /// DAP namespace
    #[arg(short, long, default_value = "canvas")]
    pub namespace: String,

    /// Single table, comma-separated list, or 'all'
    #[arg(short, long)]
    pub table: String,

    /// PostgreSQL connection string (env: DAP_CONNECTION_STRING or DATABASE_URL)
    #[arg(long, env = "DAP_CONNECTION_STRING")]
    pub connection_string: Option<String>,

    /// Metadata schema storing table_sync (default: instructure_dap)
    #[arg(long, default_value = DEFAULT_META_SCHEMA)]
    pub meta_schema: String,
}

#[derive(Debug, Args)]
pub struct SkillArgs {
    /// Install SKILL.md into <dir>/dapkeel/SKILL.md instead of printing to stdout
    #[arg(long)]
    pub install: Option<PathBuf>,
}
