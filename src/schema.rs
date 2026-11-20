use crate::error::DapError;
use crate::types::VersionedSchema;
use indexmap::IndexMap;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct ColumnSpec {
    pub name: String,
    pub is_primary_key: bool,
    pub nullable: bool,
    pub pg_type: String,
    pub keel_type: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub enum_values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableSpec {
    pub namespace: String,
    pub table: String,
    pub schema_version: i32,
    pub description: Option<String>,
    pub primary_keys: Vec<String>,
    pub columns: Vec<ColumnSpec>,
}

/// Parse an Instructure DAP `VersionedSchema` into a flat relational `TableSpec`.
/// Supports both the canonical DAP envelope schema (`properties.key` + `properties.value`)
/// and flat JSON Schemas (`properties.<col>`).
pub fn parse_dap_schema(
    namespace: &str,
    table: &str,
    vs: &VersionedSchema,
) -> Result<TableSpec, DapError> {
    let root_obj = vs
        .schema
        .as_object()
        .ok_or_else(|| DapError::schema("DAP schema root must be a JSON object"))?;

    let description = root_obj
        .get("description")
        .or_else(|| root_obj.get("title"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let props = root_obj
        .get("properties")
        .and_then(|v| v.as_object())
        .ok_or_else(|| DapError::schema("DAP schema missing 'properties' object"))?;

    let defs = root_obj
        .get("$defs")
        .or_else(|| root_obj.get("definitions"))
        .and_then(|v| v.as_object());

    let mut columns: IndexMap<String, ColumnSpec> = IndexMap::new();
    let mut primary_keys: Vec<String> = Vec::new();

    if props.contains_key("key") || props.contains_key("value") {
        // Canonical DAP schema: key + value (+ meta)
        if let Some(key_schema) = props.get("key") {
            let resolved_key = resolve_ref(key_schema, defs);
            let key_required = required_set(resolved_key);
            if let Some(key_props) = resolved_key.get("properties").and_then(|v| v.as_object()) {
                for (col_name, col_val) in key_props {
                    let resolved = resolve_ref(col_val, defs);
                    let mut col = parse_column(col_name, resolved, &key_required, true);
                    col.nullable = false;
                    col.is_primary_key = true;
                    primary_keys.push(col_name.clone());
                    columns.insert(col_name.clone(), col);
                }
            }
        }
        if let Some(val_schema) = props.get("value") {
            let resolved_val = resolve_ref(val_schema, defs);
            let val_required = required_set(resolved_val);
            if let Some(val_props) = resolved_val.get("properties").and_then(|v| v.as_object()) {
                for (col_name, col_val) in val_props {
                    if columns.contains_key(col_name) {
                        continue;
                    }
                    let resolved = resolve_ref(col_val, defs);
                    let col = parse_column(col_name, resolved, &val_required, false);
                    columns.insert(col_name.clone(), col);
                }
            }
        }
    } else {
        // Flat JSON Schema fallback
        let req = required_set(&vs.schema);
        for (col_name, col_val) in props {
            if col_name == "meta" {
                continue;
            }
            let resolved = resolve_ref(col_val, defs);
            let is_pk = col_name == "id";
            let col = parse_column(col_name, resolved, &req, is_pk);
            if is_pk {
                primary_keys.push(col_name.clone());
            }
            columns.insert(col_name.clone(), col);
        }
    }

    if columns.is_empty() {
        return Err(DapError::schema(format!(
            "No columns could be extracted from DAP schema for '{namespace}.{table}'"
        )));
    }

    Ok(TableSpec {
        namespace: namespace.to_string(),
        table: table.to_string(),
        schema_version: vs.version,
        description,
        primary_keys,
        columns: columns.into_values().collect(),
    })
}

fn resolve_ref<'a>(val: &'a Value, defs: Option<&'a serde_json::Map<String, Value>>) -> &'a Value {
    if let Some(r) = val.get("$ref").and_then(|v| v.as_str())
        && let Some(defs_map) = defs
    {
        let key = r
            .strip_prefix("#/$defs/")
            .or_else(|| r.strip_prefix("#/definitions/"))
            .unwrap_or(r);
        if let Some(target) = defs_map.get(key) {
            return target;
        }
    }
    val
}

fn required_set(obj: &Value) -> HashSet<String> {
    obj.get("required")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_column(
    name: &str,
    schema: &Value,
    required: &HashSet<String>,
    is_primary_key: bool,
) -> ColumnSpec {
    let mut nullable = !is_primary_key && !required.contains(name);

    // Handle oneOf / anyOf with null branch
    let effective = if let Some(variants) = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(|v| v.as_array())
    {
        let mut non_null = None;
        for v in variants {
            if v.get("type").and_then(|t| t.as_str()) == Some("null") {
                nullable = true;
            } else if non_null.is_none() {
                non_null = Some(v);
            }
        }
        non_null.unwrap_or(schema)
    } else {
        schema
    };

    let json_type = match effective.get("type") {
        Some(Value::String(s)) => s.as_str(),
        Some(Value::Array(arr)) => {
            if arr.iter().any(|v| v.as_str() == Some("null")) {
                nullable = true;
            }
            arr.iter()
                .filter_map(|v| v.as_str())
                .find(|t| *t != "null")
                .unwrap_or("string")
        }
        _ => "string",
    };

    let format = effective.get("format").and_then(|v| v.as_str());
    let enum_values: Vec<String> = effective
        .get("enum")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();

    let description = schema
        .get("description")
        .or_else(|| schema.get("title"))
        .or_else(|| effective.get("description"))
        .or_else(|| effective.get("title"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let (pg_type, keel_type) = match (json_type, format) {
        ("integer", Some("int32")) => ("INTEGER", "integer"),
        ("integer", _) => ("BIGINT", "integer"),
        ("number", _) => ("DOUBLE PRECISION", "float"),
        ("boolean", _) => ("BOOLEAN", "boolean"),
        ("string", Some("date-time")) => ("TIMESTAMPTZ", "timestamp"),
        ("string", Some("date")) => ("DATE", "date"),
        ("string", Some("uuid")) => ("UUID", "string"),
        ("object", _) | ("array", _) => ("JSONB", "json"),
        _ => ("TEXT", "string"),
    };

    ColumnSpec {
        name: name.to_string(),
        is_primary_key,
        nullable: if is_primary_key { false } else { nullable },
        pg_type: pg_type.to_string(),
        keel_type: keel_type.to_string(),
        enum_values,
        description,
    }
}

/// Convert a parsed DAP `TableSpec` into an ODCS v3.2.0 YAML contract
/// compatible with `datakeel` and `sqlkeel --schema`.
pub fn to_odcs_yaml(spec: &TableSpec) -> Result<String, DapError> {
    let mut props = Vec::new();
    for col in &spec.columns {
        let mut p = serde_yaml_ng::Mapping::new();
        p.insert("name".into(), col.name.clone().into());
        p.insert("logicalType".into(), col.keel_type.clone().into());
        p.insert("physicalType".into(), col.pg_type.clone().into());
        p.insert("required".into(), (!col.nullable).into());
        if col.is_primary_key {
            p.insert("primaryKey".into(), true.into());
            p.insert("unique".into(), true.into());
        }
        if let Some(desc) = &col.description {
            p.insert("description".into(), desc.clone().into());
        }
        if !col.enum_values.is_empty() {
            let mut opts = serde_yaml_ng::Mapping::new();
            let vals: Vec<serde_yaml_ng::Value> =
                col.enum_values.iter().cloned().map(Into::into).collect();
            opts.insert("enum".into(), serde_yaml_ng::Value::Sequence(vals));
            p.insert(
                "logicalTypeOptions".into(),
                serde_yaml_ng::Value::Mapping(opts),
            );
        }
        props.push(serde_yaml_ng::Value::Mapping(p));
    }

    let mut table_obj = serde_yaml_ng::Mapping::new();
    table_obj.insert("name".into(), spec.table.clone().into());
    table_obj.insert(
        "physicalName".into(),
        format!("{}.{}", spec.namespace, spec.table).into(),
    );
    if let Some(desc) = &spec.description {
        table_obj.insert("description".into(), desc.clone().into());
    }
    table_obj.insert("properties".into(), serde_yaml_ng::Value::Sequence(props));

    let mut root = serde_yaml_ng::Mapping::new();
    root.insert("apiVersion".into(), "v3.2.0".into());
    root.insert("kind".into(), "DataContract".into());
    root.insert(
        "id".into(),
        format!("dap-{}-{}", spec.namespace, spec.table).into(),
    );
    root.insert(
        "name".into(),
        format!("{} {}", spec.namespace, spec.table).into(),
    );
    root.insert(
        "version".into(),
        format!("{}.0.0", spec.schema_version).into(),
    );
    root.insert("status".into(), "active".into());
    root.insert(
        "schema".into(),
        serde_yaml_ng::Value::Sequence(vec![serde_yaml_ng::Value::Mapping(table_obj)]),
    );

    serde_yaml_ng::to_string(&root)
        .map_err(|e| DapError::schema(format!("Failed to serialize ODCS YAML: {e}")))
}

/// Convert a parsed DAP `TableSpec` into a compact `datakeel.toml` contract
/// compatible with `datakeel` and `sqlkeel --schema`.
pub fn to_datakeel_toml(spec: &TableSpec) -> String {
    let mut out = String::new();
    out.push_str("[contract]\n");
    out.push_str(&format!("name = \"{}_{}\"\n", spec.namespace, spec.table));
    out.push_str(&format!("version = \"{}.0.0\"\n", spec.schema_version));
    out.push_str(&format!("table = \"{}\"\n", spec.table));
    if let Some(desc) = &spec.description {
        out.push_str(&format!(
            "description = {}\n",
            serde_json::to_string(desc).unwrap_or_else(|_| "\"\"".to_string())
        ));
    }
    out.push('\n');

    for col in &spec.columns {
        out.push_str("[[columns]]\n");
        out.push_str(&format!("name = \"{}\"\n", col.name));
        out.push_str(&format!("type = \"{}\"\n", col.keel_type));
        out.push_str(&format!("nullable = {}\n", col.nullable));
        if col.is_primary_key {
            out.push_str("primary_key = true\n");
            out.push_str("unique = true\n");
        }
        if !col.enum_values.is_empty() {
            let quoted: Vec<String> = col
                .enum_values
                .iter()
                .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "\"\"".to_string()))
                .collect();
            out.push_str(&format!("allowed_values = [{}]\n", quoted.join(", ")));
        }
        out.push('\n');
    }
    out
}

pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

pub fn create_table_ddl(target_schema: &str, target_table: &str, spec: &TableSpec) -> String {
    let mut defs: Vec<String> = spec
        .columns
        .iter()
        .map(|c| {
            let null_clause = if c.nullable { "" } else { " NOT NULL" };
            format!("    {} {}{}", quote_ident(&c.name), c.pg_type, null_clause)
        })
        .collect();

    if !spec.primary_keys.is_empty() {
        let pks: Vec<String> = spec.primary_keys.iter().map(|k| quote_ident(k)).collect();
        defs.push(format!("    PRIMARY KEY ({})", pks.join(", ")));
    }

    format!(
        "CREATE TABLE IF NOT EXISTS {}.{} (\n{}\n);",
        quote_ident(target_schema),
        quote_ident(target_table),
        defs.join(",\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_canonical_dap_schema_and_exports_contracts() {
        let vs = VersionedSchema {
            version: 3,
            schema: json!({
                "type": "object",
                "title": "Canvas users table",
                "properties": {
                    "key": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "integer", "format": "int64", "title": "Primary key" }
                        },
                        "required": ["id"]
                    },
                    "value": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "workflow_state": { "type": "string", "enum": ["registered", "deleted"] },
                            "created_at": { "type": "string", "format": "date-time" },
                            "settings": { "type": "object" }
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
            }),
        };

        let spec = parse_dap_schema("canvas", "users", &vs).expect("parse");
        assert_eq!(spec.primary_keys, vec!["id"]);
        assert_eq!(spec.columns.len(), 5);
        let by_name = |n: &str| spec.columns.iter().find(|c| c.name == n).unwrap();
        assert_eq!(by_name("id").pg_type, "BIGINT");
        assert!(!by_name("id").nullable);
        assert_eq!(
            by_name("workflow_state").enum_values,
            vec!["registered", "deleted"]
        );
        assert_eq!(by_name("created_at").pg_type, "TIMESTAMPTZ");
        assert_eq!(by_name("settings").pg_type, "JSONB");

        let odcs = to_odcs_yaml(&spec).expect("odcs");
        assert!(odcs.contains("apiVersion: v3.2.0"));
        assert!(odcs.contains("primaryKey: true"));

        let toml_str = to_datakeel_toml(&spec);
        assert!(toml_str.contains("primary_key = true"));
        assert!(toml_str.contains("allowed_values = [\"registered\", \"deleted\"]"));
    }
}
