//! OSDL Model Context Protocol (MCP) server.
//!
//! Wraps `osdl-core` so AI agents (Claude, Cursor, Copilot, …) can read,
//! validate, format and transpile an OSDL schema over the Model Context
//! Protocol. The transport is JSON-RPC 2.0 over stdio; no external MCP SDK is
//! required, which keeps the server fully self-contained and deterministic.
//!
//! Exposed tools:
//! * `read_schema`     — parse a `.osdl` file and return a structured model list.
//! * `validate_schema` — parse + validate, returning structured diagnostics.
//! * `format_schema`   — canonicalise an `.osdl` file in place (or return result).
//! * `build`           — run a [`CodeRenderer`] for a target, return file map.
//! * `lint`            — run the schema lint rules, return structured findings.
//! * `migrate_preview` — read-only diff against an optional current lockfile,
//!   returning the migration plan + up/down SQL for safe agent review.
//!
//! Every tool takes a `path` argument (an `.osdl` file) and returns JSON the
//! agent can reason about directly.

#![allow(clippy::result_large_err)]

use osdl_core::Target;
use osdl_core::validator::CodeRenderer;
use osdl_parser::parse;
use serde_json::{Value, json};
use std::str::FromStr as _;

/// Run the MCP server, reading newline-delimited JSON-RPC requests from stdin
/// and writing JSON-RPC responses to stdout. This is the `osdl mcp` entry point.
pub fn run_stdio() -> std::io::Result<()> {
    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| std::io::Error::other(format!("tokio runtime: {e}")))?;
    runtime.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin).lines();
        let mut out = stdout;
        while let Some(line) = reader
            .next_line()
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?
        {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let response = handle_message(line);
            let serialized = serde_json::to_string(&response)
                .map_err(|e| std::io::Error::other(format!("serialize: {e}")))?;
            out.write_all(format!("{serialized}\n").as_bytes())
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            out.flush()
                .await
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        Ok(())
    })
}

/// Parse a single JSON-RPC request and produce a response (or a notification
/// acknowledgement). The MCP framing is handled by the caller.
fn handle_message(line: &str) -> Value {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": -32700, "message": format!("parse error: {e}") }
            });
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    match method {
        "initialize" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "osdl", "version": env!("CARGO_PKG_VERSION") }
            }
        }),
        "notifications/initialized" => Value::Null,
        "tools/list" => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "tools": tools_list() }
        }),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            let result = dispatch_tool(name, &args);
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "content": [ { "type": "text", "text": serde_json::to_string_pretty(&result).unwrap_or_else(|_| "null".into()) } ],
                    "isError": result.get("error").is_some()
                }
            })
        }
        _ => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("method not found: {method}") }
        }),
    }
}

/// The MCP tool catalog.
fn tools_list() -> Vec<Value> {
    vec![
        tool(
            "read_schema",
            "Parse an OSDL schema file and return its models and fields as structured JSON.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the .osdl file" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "validate_schema",
            "Parse and validate an OSDL schema, returning structured diagnostics an agent can fix.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the .osdl file" },
                    "target": { "type": "string", "description": "Backend target (seaorm-sqlite, mongo, typescript, graphql, openapi)" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "format_schema",
            "Deterministically reformat an OSDL file in place (and return the canonical text).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the .osdl file" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "build",
            "Transpile an OSDL schema to a target and return the generated file map.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the .osdl file" },
                    "target": { "type": "string", "description": "Backend target (seaorm-sqlite, mongo, typescript, graphql, openapi)" }
                },
                "required": ["path", "target"]
            }),
        ),
        tool(
            "lint",
            "Run the schema lint rules and return structured findings an agent can fix.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the .osdl file" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "migrate_preview",
            "Read-only: diff the schema against an optional current lockfile and return the migration plan + up/down SQL so an agent can review a proposed change safely.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path to the .osdl file" },
                    "current_path": { "type": "string", "description": "Optional path to a .lock file to diff against (omit for a fresh schema)" },
                    "target": { "type": "string", "description": "Backend target (seaorm-sqlite, seaorm-postgres, seaorm-mysql, mongo)" }
                },
                "required": ["path"]
            }),
        ),
    ]
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": input_schema
    })
}

/// Dispatch a tool call by name.
fn dispatch_tool(name: &str, args: &Value) -> Value {
    match name {
        "read_schema" => tool_read_schema(args),
        "validate_schema" => tool_validate_schema(args),
        "format_schema" => tool_format_schema(args),
        "build" => tool_build(args),
        "lint" => tool_lint(args),
        "migrate_preview" => tool_migrate_preview(args),
        _ => json!({ "error": format!("unknown tool: {name}") }),
    }
}

fn arg_path(args: &Value) -> Option<String> {
    args.get("path")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
}

fn parse_target(args: &Value) -> Target {
    args.get("target")
        .and_then(|t| t.as_str())
        .and_then(|s| Target::from_str(s).ok())
        .unwrap_or(Target::SeaOrmSqlite)
}

fn tool_read_schema(args: &Value) -> Value {
    let Some(path) = arg_path(args) else {
        return json!({ "error": "missing 'path'" });
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": format!("reading {path}: {e}") }),
    };
    let ast = match parse(&src) {
        Ok(a) => a,
        Err(e) => return json!({ "error": format!("parse error: {e}") }),
    };
    let mut models = Vec::new();
    for (_, m) in ast.models() {
        let mut fields = Vec::new();
        for (_, f) in m.fields() {
            fields.push(json!({
                "name": f.name,
                "type": f.type_keyword(),
                "intents": f.intents.iter().map(|i| i.as_keyword()).collect::<Vec<_>>(),
                "line": f.line,
            }));
        }
        models.push(json!({ "name": m.name, "line": m.line, "fields": fields }));
    }
    json!({ "path": path, "models": models })
}

fn tool_validate_schema(args: &Value) -> Value {
    let Some(path) = arg_path(args) else {
        return json!({ "error": "missing 'path'" });
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": format!("reading {path}: {e}") }),
    };
    let ast = match parse(&src) {
        Ok(a) => a,
        Err(e) => {
            return json!({ "valid": false, "diagnostics": [ { "severity": "error", "message": format!("{e}") } ] });
        }
    };
    match osdl_core::Validator::validate(&ast, Some(parse_target(args))) {
        Ok(()) => json!({ "valid": true, "diagnostics": [] }),
        Err(e) => json!({
            "valid": false,
            "diagnostics": [ { "severity": "error", "message": format!("{e}") } ]
        }),
    }
}

fn tool_format_schema(args: &Value) -> Value {
    let Some(path) = arg_path(args) else {
        return json!({ "error": "missing 'path'" });
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": format!("reading {path}: {e}") }),
    };
    let ast = match parse(&src) {
        Ok(a) => a,
        Err(e) => return json!({ "error": format!("parse error: {e}") }),
    };
    if let Err(e) = osdl_core::Validator::validate(&ast, Some(Target::SeaOrmSqlite)) {
        return json!({ "error": format!("validate: {e}") });
    }
    let formatted = osdl_core::formatter::format_ast(&ast);
    if let Err(e) = std::fs::write(&path, &formatted) {
        return json!({ "error": format!("writing {path}: {e}") });
    }
    json!({ "path": path, "formatted": formatted })
}

fn tool_build(args: &Value) -> Value {
    let Some(path) = arg_path(args) else {
        return json!({ "error": "missing 'path'" });
    };
    let target = parse_target(args);
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": format!("reading {path}: {e}") }),
    };
    let ast = match parse(&src) {
        Ok(a) => a,
        Err(e) => return json!({ "error": format!("parse error: {e}") }),
    };
    if let Err(e) = osdl_core::Validator::validate(&ast, Some(target)) {
        return json!({ "error": format!("validate: {e}") });
    }
    let files = match render_target(target, &ast) {
        Ok(f) => f,
        Err(e) => return json!({ "error": format!("{e}") }),
    };
    let map: serde_json::Map<String, Value> = files
        .into_iter()
        .map(|(rel, contents)| (rel, Value::String(contents)))
        .collect();
    json!({ "target": target.as_str(), "files": Value::Object(map) })
}

fn tool_lint(args: &Value) -> Value {
    let Some(path) = arg_path(args) else {
        return json!({ "error": "missing 'path'" });
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": format!("reading {path}: {e}") }),
    };
    let ast = match parse(&src) {
        Ok(a) => a,
        Err(e) => return json!({ "error": format!("parse error: {e}") }),
    };
    let linter = osdl_core::lint::Linter::new(osdl_core::lint::LintConfig::default());
    let findings = linter.lint(&ast);
    let findings_json: Vec<Value> = findings
        .iter()
        .map(|f| {
            json!({
                "rule": f.rule.as_str(),
                "severity": f.severity.as_str(),
                "message": f.message,
            })
        })
        .collect();
    let max_severity = findings
        .iter()
        .map(|f| f.severity)
        .max()
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|| "info".to_string());
    json!({
        "path": path,
        "findings": findings_json,
        "max_severity": max_severity,
        "ok": findings_json.is_empty(),
    })
}

/// Read-only migration preview: diff the schema against an optional current
/// lockfile and return the plan + up/down SQL so an agent can review a proposed
/// change safely. Never applies anything to a database.
fn tool_migrate_preview(args: &Value) -> Value {
    let Some(path) = arg_path(args) else {
        return json!({ "error": "missing 'path'" });
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => return json!({ "error": format!("reading {path}: {e}") }),
    };
    let target = parse_target(args);
    let ast = match parse(&src) {
        Ok(a) => a,
        Err(e) => return json!({ "error": format!("parse error: {e}") }),
    };
    if let Err(e) = osdl_core::Validator::validate(&ast, Some(target)) {
        return json!({ "error": format!("validate: {e}") });
    }
    let target_lock = osdl_core::lockfile::Lockfile::from_ast(&ast);
    // The "current" schema we are migrating from. Without a supplied lockfile
    // this is an empty schema (a fresh `init`): every model becomes a CreateModel.
    let current = match args.get("current_path").and_then(|p| p.as_str()) {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(text) => match osdl_core::lockfile::Lockfile::from_str(&text) {
                Ok(lf) => lf,
                Err(e) => return json!({ "error": format!("parsing current lockfile {p}: {e}") }),
            },
            Err(e) => return json!({ "error": format!("reading current lockfile {p}: {e}") }),
        },
        None => osdl_core::lockfile::Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![],
        },
    };
    let plan = match osdl_migrator::plan_migration(&current, &ast) {
        Ok(p) => p,
        Err(e) => return json!({ "error": format!("planning migration: {e}") }),
    };
    let ops: Vec<Value> = plan
        .ops
        .iter()
        .map(|op| json!({ "op": format!("{op:?}") }))
        .collect();
    // SQL is only meaningful for the SQL backends.
    let sql = match target {
        Target::SeaOrmSqlite | Target::SeaOrmPostgres | Target::SeaOrmMysql => {
            let dialect = match target {
                Target::SeaOrmSqlite => osdl_adapter::sql::SqlDialect::Sqlite,
                Target::SeaOrmPostgres => osdl_adapter::sql::SqlDialect::Postgres,
                Target::SeaOrmMysql => osdl_adapter::sql::SqlDialect::Mysql,
                _ => unreachable!(),
            };
            let up: Vec<String> = plan
                .ops
                .iter()
                .flat_map(|op| {
                    osdl_adapter::sql::op_to_sql(dialect, op, &target_lock, Some(&current))
                        .unwrap_or_else(|e| vec![format!("-- ERROR: {e}")])
                })
                .collect();
            let down = osdl_adapter::migrate::render_down_sql(
                dialect,
                &plan,
                &target_lock,
                Some(&current),
            );
            Some(json!({
                "dialect": format!("{dialect:?}"),
                "up": up,
                "down": down,
            }))
        }
        _ => None,
    };
    json!({
        "path": path,
        "target": target.as_str(),
        "ops": ops,
        "sql": sql,
        "op_count": ops.len(),
    })
}

/// Render a schema with the appropriate [`CodeRenderer`] for `target`.
fn render_target(
    target: Target,
    ast: &osdl_core::ast::Ast,
) -> Result<Vec<(String, String)>, osdl_core::errors::OsdlError> {
    match target {
        Target::SeaOrmSqlite | Target::SeaOrmPostgres | Target::SeaOrmMysql => {
            osdl_codegen_seaorm::SeaOrmRenderer::new(target).render(ast)
        }
        Target::Mongo => osdl_codegen_mongo::MongoRenderer::new(target).render(ast),
        Target::TypeScript => osdl_codegen_typescript::TypeScriptRenderer::new(target).render(ast),
        Target::GraphQl => osdl_codegen_graphql::GraphQLRenderer::new(target).render(ast),
        Target::OpenApi => osdl_codegen_openapi::OpenApiRenderer::new(target).render(ast),
        Target::JsonSchema => osdl_codegen_jsonschema::JsonSchemaRenderer::new(target).render(ast),
        Target::Zod => osdl_codegen_ts_validators::TsValidatorRenderer::new(
            target,
            osdl_codegen_ts_validators::ValidatorFlavor::Zod,
        )
        .render(ast),
        Target::Valibot => osdl_codegen_ts_validators::TsValidatorRenderer::new(
            target,
            osdl_codegen_ts_validators::ValidatorFlavor::Valibot,
        )
        .render(ast),
        Target::TypeBox => osdl_codegen_ts_validators::TsValidatorRenderer::new(
            target,
            osdl_codegen_ts_validators::ValidatorFlavor::TypeBox,
        )
        .render(ast),
        Target::Trpc => osdl_codegen_trpc::TrpcRenderer::new(target).render(ast),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_list_is_nonempty_and_named() {
        let tools = tools_list();
        assert!(!tools.is_empty());
        assert!(tools.iter().all(|t| t.get("name").is_some()));
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"read_schema"));
        assert!(names.contains(&"validate_schema"));
        assert!(names.contains(&"format_schema"));
        assert!(names.contains(&"build"));
        assert!(names.contains(&"lint"));
        assert!(names.contains(&"migrate_preview"));
    }

    #[test]
    fn initialize_returns_server_info() {
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        assert_eq!(resp["result"]["serverInfo"]["name"], "osdl");
    }

    #[test]
    fn unknown_method_errors() {
        let resp = handle_message(r#"{"jsonrpc":"2.0","id":2,"method":"bogus","params":{}}"#);
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[test]
    fn read_schema_returns_models() {
        let dir = std::env::temp_dir();
        let p = dir.join("mcp_read_test.osdl");
        std::fs::write(&p, "User\n  id uuid -pk\n  email string -uniq\n").unwrap();
        let args = json!({ "path": p.to_string_lossy() });
        let resp = dispatch_tool("read_schema", &args);
        assert!(resp["models"].as_array().unwrap().len() == 1);
        assert_eq!(resp["models"][0]["name"], "User");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn validate_schema_reports_errors() {
        let dir = std::env::temp_dir();
        let p = dir.join("mcp_validate_test.osdl");
        std::fs::write(&p, "Post\n  id uuid -pk\n  author User.id\n").unwrap();
        let args = json!({ "path": p.to_string_lossy() });
        let resp = dispatch_tool("validate_schema", &args);
        assert_eq!(resp["valid"], false);
        assert!(!resp["diagnostics"].as_array().unwrap().is_empty());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn lint_returns_findings_for_dirty_schema() {
        let dir = std::env::temp_dir();
        let p = dir.join("mcp_lint_test.osdl");
        // No doc comment + no timestamps -> MissingModelDoc / MissingTimestamps warnings.
        std::fs::write(&p, "userprofile\n  id uuid -pk\n  email string\n").unwrap();
        let args = json!({ "path": p.to_string_lossy() });
        let resp = dispatch_tool("lint", &args);
        assert!(!resp["findings"].as_array().unwrap().is_empty());
        assert!(resp["ok"] == false || resp["max_severity"].is_string());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn migrate_preview_plans_create_models() {
        let dir = std::env::temp_dir();
        let p = dir.join("mcp_preview_test.osdl");
        std::fs::write(&p, "User\n  id uuid -pk\n  email string -uniq\n").unwrap();
        let args = json!({ "path": p.to_string_lossy(), "target": "seaorm-postgres" });
        let resp = dispatch_tool("migrate_preview", &args);
        assert_eq!(resp["op_count"], 1);
        let sql = resp["sql"].as_object().unwrap();
        assert!(!sql["up"].as_array().unwrap().is_empty());
        // Fresh schema: the down of a CreateModel is a DROP TABLE (rolling back a create).
        let down = sql["down"].as_array().unwrap();
        assert!(
            down.first()
                .is_some_and(|s| s.as_str().unwrap_or("").contains("DROP TABLE")),
            "down of a fresh CreateModel should be a DROP TABLE, got: {down:?}"
        );
        let _ = std::fs::remove_file(&p);
    }
}
