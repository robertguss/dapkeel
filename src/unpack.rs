use crate::error::DapError;
use flate2::read::MultiGzDecoder;
use serde_json::{Map, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct UnpackedRecord {
    pub action: Option<String>,
    pub flat: Map<String, Value>,
}

/// Unpack a single DAP JSONL record (`{"key": {...}, "value": {...}, "meta": {...}, "schema_version": N}`)
/// into a flat JSON object suitable for `datakeel` / `loadkeel` / Postgres `COPY`.
pub fn unpack_dap_json_line(line: &str, include_meta: bool) -> Result<UnpackedRecord, DapError> {
    let parsed: Value = serde_json::from_str(line)
        .map_err(|e| DapError::schema(format!("Invalid JSONL record from DAP: {e}")))?;
    let obj = parsed
        .as_object()
        .ok_or_else(|| DapError::schema("DAP JSONL line is not a JSON object"))?;

    let schema_version = obj.get("schema_version").and_then(|v| v.as_i64());
    let meta = obj.get("meta").and_then(|v| v.as_object());
    let ts = meta
        .and_then(|m| m.get("ts"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let action = meta
        .and_then(|m| m.get("action"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let mut flat = Map::new();

    if obj.contains_key("key") || obj.contains_key("value") {
        if let Some(k) = obj.get("key").and_then(|v| v.as_object()) {
            for (name, val) in k {
                flat.insert(name.clone(), val.clone());
            }
        }
        if let Some(v) = obj.get("value").and_then(|v| v.as_object()) {
            for (name, val) in v {
                flat.entry(name.clone()).or_insert_with(|| val.clone());
            }
        }
    } else {
        // Already flat fallback
        for (name, val) in obj {
            if name != "meta" && name != "schema_version" {
                flat.insert(name.clone(), val.clone());
            }
        }
    }

    if include_meta {
        if let Some(t) = &ts {
            flat.insert("_dap_ts".to_string(), Value::String(t.clone()));
        }
        if let Some(a) = &action {
            flat.insert("_dap_action".to_string(), Value::String(a.clone()));
        }
        if let Some(sv) = schema_version {
            flat.insert("_schema_version".to_string(), Value::from(sv));
        }
    }

    Ok(UnpackedRecord { action, flat })
}

/// Open a possibly `.gz`-compressed file for buffered line reading.
pub fn open_maybe_gzip(path: &Path) -> Result<Box<dyn BufRead>, DapError> {
    let file = File::open(path)
        .map_err(|e| DapError::io(format!("Failed to open '{}': {e}", path.display())))?;

    let is_gz = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("gz"))
        .unwrap_or(false);

    if is_gz {
        let dec = MultiGzDecoder::new(file);
        Ok(Box::new(BufReader::new(dec)))
    } else {
        // Peek gzip magic bytes (0x1f 0x8b) just in case filename omitted .gz
        let mut reader = BufReader::new(file);
        let buf = reader
            .fill_buf()
            .map_err(|e| DapError::io(format!("Failed to read '{}': {e}", path.display())))?;
        if buf.len() >= 2 && buf[0] == 0x1f && buf[1] == 0x8b {
            let dec = MultiGzDecoder::new(reader);
            Ok(Box::new(BufReader::new(dec)))
        } else {
            Ok(Box::new(reader))
        }
    }
}

/// Read all DAP JSONL files (gzipped or plain) and stream unpacked JSONL lines to `writer`.
/// Returns total rows unpacked.
pub fn unpack_files_to_writer<W: Write>(
    files: &[std::path::PathBuf],
    writer: &mut W,
    include_meta: bool,
) -> Result<u64, DapError> {
    let mut count = 0u64;
    for path in files {
        let mut reader = open_maybe_gzip(path)?;
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .map_err(|e| DapError::io(format!("Failed reading '{}': {e}", path.display())))?;
            if n == 0 {
                break;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let rec = unpack_dap_json_line(trimmed, include_meta)?;
            let serialized = serde_json::to_string(&Value::Object(rec.flat))
                .map_err(|e| DapError::io(format!("Failed serializing unpacked JSONL: {e}")))?;
            writer
                .write_all(serialized.as_bytes())
                .and_then(|_| writer.write_all(b"\n"))
                .map_err(|e| DapError::io(format!("Failed writing unpacked JSONL: {e}")))?;
            count += 1;
        }
    }
    writer
        .flush()
        .map_err(|e| DapError::io(format!("Failed flushing unpacked output: {e}")))?;
    Ok(count)
}

/// Decompress gzipped bytes into a file or return raw reader helper.
#[allow(dead_code)]
pub fn decompress_gzip_file(src: &Path, dst: &Path) -> Result<u64, DapError> {
    let input = File::open(src)
        .map_err(|e| DapError::io(format!("Failed to open '{}': {e}", src.display())))?;
    let mut decoder = MultiGzDecoder::new(input);
    let mut output = File::create(dst)
        .map_err(|e| DapError::io(format!("Failed to create '{}': {e}", dst.display())))?;
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = decoder
            .read(&mut buf)
            .map_err(|e| DapError::io(format!("Gzip decompress failed: {e}")))?;
        if n == 0 {
            break;
        }
        output
            .write_all(&buf[..n])
            .map_err(|e| DapError::io(format!("Write failed for '{}': {e}", dst.display())))?;
        total += n as u64;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_dap_envelope_preserving_keys_and_meta() {
        let line = r#"{"schema_version":2,"key":{"id":42},"value":{"name":"Ada","workflow_state":"active"},"meta":{"ts":"2026-11-20T00:00:00Z","action":"U"}}"#;
        let rec = unpack_dap_json_line(line, true).expect("unpack");
        assert_eq!(rec.action.as_deref(), Some("U"));
        assert_eq!(rec.flat.get("id").and_then(|v| v.as_i64()), Some(42));
        assert_eq!(rec.flat.get("name").and_then(|v| v.as_str()), Some("Ada"));
        assert_eq!(
            rec.flat.get("_dap_action").and_then(|v| v.as_str()),
            Some("U")
        );
        assert_eq!(
            rec.flat.get("_schema_version").and_then(|v| v.as_i64()),
            Some(2)
        );
    }
}
