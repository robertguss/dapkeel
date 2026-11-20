---
name: dapkeel
description: Rust CLI for Instructure Data Access Platform (Canvas Data 2) with OAuth2 per-table JWTs, ODCS/TOML contract export, streaming JSONL unpack, and atomic PostgreSQL COPY replication.
---

# dapkeel — Agent Instructions

Use `dapkeel` to query Instructure's Data Access Platform (Canvas Data 2), export table schemas into `datakeel` / `sqlkeel` contracts, stream unpacked JSONL into the Keel Data Stack, or replicate Canvas tables directly into PostgreSQL.

## Environment Variables (never hardcode secrets)

- `DAP_API_URL` — Regional Instructure API Gateway base URL (e.g. `https://us-east-1.api-gateway.instructure.com`)
- `DAP_CLIENT_ID` — Instructure Identity Service OAuth client ID
- `DAP_CLIENT_SECRET` — Instructure Identity Service OAuth client secret
- `DAP_CONNECTION_STRING` (or fallback `DATABASE_URL`) — PostgreSQL URL for `initdb`, `syncdb`, `listdb`, `dropdb`

## Always Pass `--format agent`

When invoking `dapkeel` programmatically, pass `--format agent` (or let non-TTY auto-detection emit compact JSON). Exit codes: `0` success, `1` operational/API/DB failure, `2` usage/config error.

## Core Commands

```bash
# Discover namespaces & tables
dapkeel namespaces --format agent
dapkeel list --namespace canvas --format agent

# Download schema JSON AND generate a datakeel/sqlkeel contract
dapkeel schema --namespace canvas --table courses,enrollments,users \
  --output-directory contracts/ --to-contract odcs --format agent

# Full snapshot or incremental CDC download (with --unpack for flat JSONL)
dapkeel snapshot --namespace canvas --table users \
  --output-directory staging/ --unpack --state .keel/dap_state.json --format agent

dapkeel incremental --namespace canvas --table users \
  --state .keel/dap_state.json --output-directory staging/ --unpack --format agent

# Direct atomic PostgreSQL COPY replication (compatible with instructure_dap.table_sync)
dapkeel initdb --namespace canvas --table courses,enrollments,users --format agent
dapkeel syncdb --namespace canvas --table all --format agent
dapkeel listdb --namespace canvas --format agent
dapkeel dropdb --namespace canvas --table courses --format agent
```

## Piping Into the Keel Data Stack

```bash
# Generate ODCS contract -> stream unpacked snapshot -> validate -> bulk load
dapkeel schema --namespace canvas --table users --to-contract odcs --output-directory contracts/
dapkeel snapshot --namespace canvas --table users --unpack --stdout \
  | datakeel check contracts/users.datacontract.yaml - --clean staging/users_clean.jsonl --quarantine staging/users_bad.jsonl
loadkeel load staging/users_clean.jsonl pg://staging.canvas_users --contract contracts/users.datacontract.yaml --auto-create --evolve
```

## Stable Error Codes

- `DAP_CONFIG_ERROR`: Missing env var or invalid CLI option
- `DAP_AUTH_FAILED`: OAuth2 token request rejected
- `DAP_TABLE_ACCESS_DENIED`: Token issuer did not grant requested `namespace/table`
- `DAP_SNAPSHOT_REQUIRED`: Incremental watermark too old; re-run `snapshot` or `dropdb` + `initdb`
- `DAP_JOB_FAILED`: Async DAP query job ended with `failed`
- `DAP_ALREADY_INITIALIZED` / `DAP_NOT_INITIALIZED`: Table state mismatch in `instructure_dap.table_sync`
