# `dapkeel`

**Blazing-fast, agent-first (DX + AX) Rust CLI for Instructure's Data Access Platform (Canvas Data 2).**

A single-binary Rust drop-in replacement for Python's `instructure-dap-client` (`dap` CLI), wired natively into the **Keel Data Stack** (`fetchkeel` · `dapkeel` · `datakeel` · `loadkeel` · `sqlkeel` · `runkeel`).

## Why `dapkeel`?

- **Zero Python runtime:** Single native Rust binary (`cargo install` or `uvx dapkeel`).
- **100% DAP v3 wire-compatible:** Implements Instructure's per-table OAuth 2.0 `authorization_details` JWT lifecycle, grant verification, separate unauthenticated S3 pre-signed download channel, async job polling with near-expiry safeguard, and `instructure_dap.table_sync` metadata schema.
- **Native Keel Data Stack bridge:**
  - `dapkeel schema --to-contract odcs|toml` translates Canvas DAP JSON schemas directly into **ODCS v3.2.0** or `datakeel.toml` contracts for `datakeel` and `sqlkeel`.
  - `--unpack` flattens DAP's `{"key", "value", "meta"}` JSONL envelope into clean rows (`_dap_ts`, `_dap_action`, `_schema_version`) ready to pipe into `datakeel check` and `loadkeel load`.
  - Native **PostgreSQL `COPY` + atomic `ON CONFLICT` upsert / CDC delete** replication for `initdb`, `syncdb`, `listdb`, and `dropdb`, including automatic schema evolution (`ALTER TABLE ... ADD COLUMN IF NOT EXISTS`).
- **DX + AX Dual-Mode Output:** Human-friendly colors on TTYs; strict, token-budgeted JSON on pipes or `--format agent` with stable `DAP_*` error codes and `fix_hint`s.

## Quickstart

```bash
export DAP_API_URL='https://us-east-1.api-gateway.instructure.com'
export DAP_CLIENT_ID='your_client_id'
export DAP_CLIENT_SECRET='your_client_secret'
export DAP_CONNECTION_STRING='postgresql://user:pass@localhost:5432/canvas_data'

# 1. Discover namespaces and tables
dapkeel namespaces
dapkeel list --namespace canvas

# 2. Download JSON schema AND generate an ODCS v3.2.0 contract for datakeel/sqlkeel
dapkeel schema --namespace canvas --table courses,enrollments,users \
  --output-directory contracts/ --to-contract odcs

# 3. Extract unpacked JSONL with local watermark tracking
dapkeel snapshot --namespace canvas --table users \
  --output-directory staging/ --unpack --state .keel/dap_state.json

dapkeel incremental --namespace canvas --table users \
  --state .keel/dap_state.json --output-directory staging/ --unpack

# 4. Or replicate directly into PostgreSQL (initdb -> syncdb -> listdb -> dropdb)
dapkeel initdb --namespace canvas --table courses,enrollments,users
dapkeel syncdb --namespace canvas --table all
dapkeel listdb --namespace canvas
```

## Commands

| Command | Purpose |
|---|---|
| `dapkeel namespaces` | List available DAP namespaces (`canvas`, `canvas_logs`, `catalog`, `new_quizzes`, etc.) |
| `dapkeel list` | List available tables in a namespace |
| `dapkeel schema` | Download `VersionedSchema` JSON and optionally export ODCS v3.2.0 / `datakeel.toml` contracts |
| `dapkeel snapshot` | Run a full table snapshot query, download parts concurrently, and optionally `--unpack` or update `--state` |
| `dapkeel incremental` | Run an incremental CDC query (`--since` / `--until` or automatic `--state` watermark resume) |
| `dapkeel initdb` | Create Postgres schema/table + `instructure_dap.table_sync` and bulk-load initial snapshot via `COPY` |
| `dapkeel syncdb` | Apply incremental CDC upserts (`"U"`) and deletes (`"D"`) + auto-evolve columns atomically |
| `dapkeel listdb` | Inspect local Postgres replication status, row counts, staleness, and schema versions |
| `dapkeel dropdb` | Drop replicated Postgres table(s) and remove their `table_sync` metadata |
| `dapkeel skill` | Print or install the bundled `SKILL.md` for AI coding agents |

## License

Dual-licensed under MIT OR Apache-2.0.
