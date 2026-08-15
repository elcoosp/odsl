//! OSDL Language Server.
//!
//! A real LSP implementation built on `async-lsp` (0.2, omni-trait API). It
//! parses and validates OSDL documents whenever they are opened or changed and
//! publishes `Diagnostic`s for every parse/validation error via
//! `textDocument/publishDiagnostics`.
//!
//! Diagnostics are produced by reusing the exact same pipeline as the CLI:
//! `osdl_parser::parse` then `osdl_core::Validator::validate`. This guarantees
//! that editor squiggles match `osdl build` / `osdl migrate` exactly.

use async_lsp::lsp_types::{
    self, Diagnostic, DidChangeTextDocumentParams, DidOpenTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, HoverProviderCapability,
    InitializeParams, InitializeResult, Location, MarkupContent, MarkupKind, PublishDiagnosticsParams,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
};
use async_lsp::{
    ClientSocket, LanguageClient, LanguageServer, MainLoop, ResponseError, router::Router,
};
use futures::future::BoxFuture;
use osdl_core::validator::Validator;
use osdl_parser::parse;
use std::collections::HashMap;
use std::ops::ControlFlow;

/// Shared server state: the latest text of each open document.
pub struct Backend {
    client: ClientSocket,
    /// uri -> current document text.
    docs: HashMap<String, String>,
}

impl Default for Backend {
    fn default() -> Self {
        Self {
            client: ClientSocket::new_closed(),
            docs: HashMap::new(),
        }
    }
}

impl Backend {
    /// Build a backend wired to the peer client socket.
    pub fn new(client: ClientSocket) -> Self {
        Self {
            client,
            docs: HashMap::new(),
        }
    }

    /// Parse + validate `text` and return LSP diagnostics, reusing the compiler
    /// pipeline so editor feedback matches the CLI.
    fn diagnostics_for(text: &str) -> Vec<Diagnostic> {
        let mut diags = Vec::new();

        let ast = match parse(text) {
            Ok(ast) => ast,
            Err(e) => {
                diags.push(Diagnostic {
                    range: lsp_types::Range::default(),
                    severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                    source: Some("osdl".into()),
                    message: format!("parse error: {e}"),
                    ..Default::default()
                });
                return diags;
            }
        };

        if let Err(err) = Validator::validate(&ast, None) {
            diags.push(Diagnostic {
                range: lsp_types::Range::default(),
                severity: Some(lsp_types::DiagnosticSeverity::ERROR),
                source: Some("osdl".into()),
                message: format!("{err}"),
                ..Default::default()
            });
        }
        diags
    }

    /// Publish diagnostics for `uri` given its current `text`.
    fn publish(&mut self, uri: lsp_types::Url, text: &str) {
        let diagnostics = Self::diagnostics_for(text);
        let _ = self.client.publish_diagnostics(PublishDiagnosticsParams {
            uri,
            version: None,
            diagnostics,
        });
    }

    /// If a field's type is a `Reference` to another model, return the name of
    /// the referenced model (used for go-to-definition).
    fn referenced_model(field: &osdl_core::Field) -> Option<String> {
        match &field.ty {
            osdl_core::FieldType::Ref(r) => Some(r.model.clone()),
            _ => None,
        }
    }

    /// Build a Markdown hover describing a field: its inferred Rust type and the
    /// SQL column definition it will generate.
    fn hover_for_field(field: &osdl_core::Field) -> String {
        let rust_ty = rust_type_name(field);
        let sql_def = sql_column_definition(field);
        format!(
            "**`{}`** → `{}`\n\n```sql\n{}\n```",
            field.name, rust_ty, sql_def
        )
    }
}

/// Infer the Rust type for a field (mirrors the SeaORM codegen mapping).
fn rust_type_name(field: &osdl_core::Field) -> String {
    if field.has(osdl_core::Intent::Virtual) {
        return "⚠ virtual (not stored)".to_string();
    }
    let base = match &field.ty {
        osdl_core::FieldType::Scalar(s) => match s {
            osdl_core::ScalarType::String => "String",
            osdl_core::ScalarType::Int => "i32",
            osdl_core::ScalarType::BigInt => "i64",
            osdl_core::ScalarType::Float => "f64",
            osdl_core::ScalarType::Bool => "bool",
            osdl_core::ScalarType::DateTime => "chrono::DateTime<Utc>",
            osdl_core::ScalarType::Date => "chrono::NaiveDate",
            osdl_core::ScalarType::Uuid => "Uuid",
            osdl_core::ScalarType::Json => "Json",
            osdl_core::ScalarType::Binary => "Vec<u8>",
        }
        .to_string(),
        osdl_core::FieldType::Ref(r) => format!("{}", r),
        osdl_core::FieldType::InferredRef(s) => s.clone(),
    };
    if field.has(osdl_core::Intent::Null) {
        format!("Option<{base}>")
    } else {
        base
    }
}

/// Infer the SQL column definition a field will produce (used for hover).
fn sql_column_definition(field: &osdl_core::Field) -> String {
    let col = field.name.clone();
    let ty = match &field.ty {
        osdl_core::FieldType::Scalar(s) => sql_type_for(*s).to_string(),
        osdl_core::FieldType::Ref(r) => format!("UUID REFERENCES \"{}\"(\"id\")", r.model),
        osdl_core::FieldType::InferredRef(s) => s.clone(),
    };
    let mut def = format!("\"{col}\" {ty}");
    if field.has(osdl_core::Intent::Pk) {
        def.push_str(" PRIMARY KEY");
    }
    if !field.has(osdl_core::Intent::Null) && !field.has(osdl_core::Intent::Pk) {
        def.push_str(" NOT NULL");
    }
    if field.has(osdl_core::Intent::Uniq) && !field.has(osdl_core::Intent::Pk) {
        def.push_str(" UNIQUE");
    }
    if let Some(expr) = &field.check_expr {
        def.push_str(&format!(" CHECK ({expr})"));
    }
    def
}

fn sql_type_for(s: osdl_core::ScalarType) -> &'static str {
    match s {
        osdl_core::ScalarType::String => "TEXT",
        osdl_core::ScalarType::Int => "INTEGER",
        osdl_core::ScalarType::BigInt => "BIGINT",
        osdl_core::ScalarType::Float => "DOUBLE",
        osdl_core::ScalarType::Bool => "BOOLEAN",
        osdl_core::ScalarType::DateTime => "TIMESTAMPTZ",
        osdl_core::ScalarType::Date => "DATE",
        osdl_core::ScalarType::Uuid => "UUID",
        osdl_core::ScalarType::Json => "JSONB",
        osdl_core::ScalarType::Binary => "BYTEA",
    }
}

impl LanguageServer for Backend {
    // The omni-trait mandates these associated types. When the backend is driven
    // through `Router::from_language_server`, `NotifyResult` must be
    // `ControlFlow<async_lsp::Result<()>>` and `Error` must convert from
    // `ResponseError`.
    type Error = ResponseError;
    type NotifyResult = ControlFlow<async_lsp::Result<()>>;

    fn initialize(
        &mut self,
        _: InitializeParams,
    ) -> BoxFuture<'static, std::result::Result<InitializeResult, Self::Error>> {
        Box::pin(async move {
            Ok(InitializeResult {
                capabilities: ServerCapabilities {
                    text_document_sync: Some(TextDocumentSyncCapability::Kind(
                        TextDocumentSyncKind::FULL,
                    )),
                    hover_provider: Some(HoverProviderCapability::Simple(true)),
                    definition_provider: Some(lsp_types::OneOf::Left(true)),
                    ..ServerCapabilities::default()
                },
                server_info: Some(lsp_types::ServerInfo {
                    name: "osdl-lsp".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                }),
            })
        })
    }

    fn did_open(&mut self, params: DidOpenTextDocumentParams) -> Self::NotifyResult {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        self.docs.insert(uri.to_string(), text.clone());
        self.publish(uri, &text);
        ControlFlow::Continue(())
    }

    fn did_change(&mut self, params: DidChangeTextDocumentParams) -> Self::NotifyResult {
        // FULL sync: the final content change carries the whole document.
        if let Some(change) = params.content_changes.into_iter().last() {
            let uri = params.text_document.uri;
            self.docs.insert(uri.to_string(), change.text.clone());
            self.publish(uri, &change.text);
        }
        ControlFlow::Continue(())
    }

    fn hover(&mut self, params: HoverParams) -> BoxFuture<'static, std::result::Result<Option<Hover>, Self::Error>> {
        // Clone the document text (owned) so the future is 'static and does not
        // borrow `&self`.
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        let text = self.docs.get(&uri.to_string()).cloned();
        Box::pin(async move {
            let Some(text) = text else {
                return Ok(None);
            };
            let Some(ast) = parse(&text).ok() else {
                return Ok(None);
            };
            let line = (pos.line + 1) as usize; // LSP lines are 0-based.
            let Some(field) = field_at_line(&ast, line) else {
                return Ok(None);
            };
            let contents = HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: Self::hover_for_field(&field),
            });
            Ok(Some(Hover {
                contents,
                range: None,
            }))
        })
    }

    fn definition(
        &mut self,
        params: GotoDefinitionParams,
    ) -> BoxFuture<'static, std::result::Result<Option<GotoDefinitionResponse>, Self::Error>> {
        let uri = params.text_document_position_params.text_document.uri.clone();
        let pos = params.text_document_position_params.position;
        let text = self.docs.get(&uri.to_string()).cloned();
        Box::pin(async move {
            let Some(text) = text else {
                return Ok(None);
            };
            let ast = match parse(&text) {
                Ok(a) => a,
                Err(_) => return Ok(None),
            };
            let line = (pos.line + 1) as usize;
            let Some(field) = field_at_line(&ast, line) else {
                return Ok(None);
            };
            let Some(target_model) = Self::referenced_model(&field) else {
                return Ok(None);
            };
            // Locate the target model's declaration line in the same document.
            let Some(target_line) = ast
                .models()
                .find(|(_, m)| m.name == target_model)
                .map(|(_, m)| m.line)
            else {
                return Ok(None);
            };
            let target_line_0 = (target_line.saturating_sub(1)) as u32;
            let loc = Location {
                uri: uri.clone(),
                range: lsp_types::Range {
                    start: lsp_types::Position {
                        line: target_line_0,
                        character: 0,
                    },
                    end: lsp_types::Position {
                        line: target_line_0,
                        character: 0,
                    },
                },
            };
            Ok(Some(GotoDefinitionResponse::Scalar(loc)))
        })
    }
}

/// Find the field declared on `line` (1-based), if any.
fn field_at_line(ast: &osdl_core::Ast, line: usize) -> Option<osdl_core::Field> {
    for (_, model) in ast.models() {
        for (_, f) in model.fields() {
            if f.line == line {
                return Some(f.clone());
            }
        }
    }
    None
}

/// Entry point: read JSON-RPC over stdio and drive the server on the async-io
/// executor. Logging goes to stderr so it never corrupts the stdio JSON-RPC
/// channel on stdout.
pub fn run_stdio() -> std::io::Result<()> {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .try_init();

    let stdin = async_io::Async::new(async_lsp::stdio::PipeStdin::lock()?)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let stdout = async_io::Async::new(async_lsp::stdio::PipeStdout::lock()?)
        .map_err(|e| std::io::Error::other(e.to_string()))?;

    let (main_loop, _client) =
        MainLoop::new_server(|client| Router::from_language_server(Backend::new(client)));
    async_io::block_on(main_loop.run_buffered(stdin, stdout))
        .map_err(|e: async_lsp::Error| std::io::Error::other(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_flag_parse_errors() {
        let diags = Backend::diagnostics_for("this is not valid osdl @@@");
        assert!(
            !diags.is_empty(),
            "expected a diagnostic for unparseable input"
        );
        assert_eq!(
            diags[0].severity,
            Some(lsp_types::DiagnosticSeverity::ERROR)
        );
    }

    #[test]
    fn diagnostics_flag_unresolved_reference() {
        // `author User.id` references a model `User` that does not exist.
        let src = "Post\n  id uuid -pk\n  author User.id\n";
        let diags = Backend::diagnostics_for(src);
        assert!(
            !diags.is_empty(),
            "expected a validation diagnostic for unresolved reference"
        );
        assert!(
            diags.iter().any(|d| d.message.contains("reference")
                || d.message.to_lowercase().contains("unresolved"))
        );
    }

    #[test]
    fn diagnostics_clean_for_valid_schema() {
        let src = "User\n  id uuid -pk\n  email string -uniq\n";
        let diags = Backend::diagnostics_for(src);
        assert!(
            diags.is_empty(),
            "valid schema should produce no diagnostics: {diags:?}"
        );
    }

    /// End-to-end handshake: drive the server in-process through an in-memory
    /// async transport (no OS pipes required) and assert that an `initialize`
    /// followed by a `didOpen` of an invalid schema yields a
    /// `publishDiagnostics` notification carrying the validation error.
    #[test]
    fn server_publishes_diagnostics_over_handshake() {
        async_io::block_on(async {
            // Build well-formed LSP frames programmatically so Content-Length is
            // always exact.
            fn frame(json: &str) -> String {
                format!("Content-Length: {}\r\n\r\n{}", json.len(), json)
            }
            let init = frame(
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#,
            );
            let uri = "file:///x.osdl";
            let text = "Post\n  id uuid -pk\n  author User.id\n";
            let did_open_json = serde_json::to_string(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri,
                        "languageId": "osdl",
                        "version": 1,
                        "text": text,
                    }
                }
            }))
            .unwrap();
            let did_open = frame(&did_open_json);
            let exit = frame(r#"{"jsonrpc":"2.0","method":"exit","params":null}"#);
            let input = format!("{init}{did_open}{exit}");

            let out = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));

            let (main_loop, _client) =
                MainLoop::new_server(|client| Router::from_language_server(Backend::new(client)));

            let out_writer = OutWriter(out.clone());
            // The loop returns `Err(Eof)` once stdin closes (our Cursor is
            // exhausted after the `exit` frame); that is a clean shutdown, not a
            // failure. We only care that the diagnostic was published first.
            let _ = main_loop
                .run(futures::io::Cursor::new(input.into_bytes()), out_writer)
                .await;

            let captured = out.lock().unwrap().clone();
            let text = String::from_utf8_lossy(&captured);
            assert!(
                text.contains("publishDiagnostics"),
                "expected a publishDiagnostics frame, got:\n{text}"
            );
            assert!(
                text.contains("UnresolvedReference") || text.contains("reference"),
                "diagnostic should name the unresolved reference, got:\n{text}"
            );
        });
    }

    #[test]
    fn hover_builds_rust_and_sql_for_field() {
        let src = "User\n  id uuid -pk\n  email string -uniq\n  age int -check \"age >= 18\"\n  bio string -null\n";
        let ast = parse(src).unwrap();
        // Find the `age` field (line 4 in 1-based; fields start at line 3).
        let age = ast
            .models()
            .flat_map(|(_, m)| m.fields())
            .find(|(_, f)| f.name == "age")
            .map(|(_, f)| f.clone())
            .unwrap();
        let hover = Backend::hover_for_field(&age);
        assert!(hover.contains("`i32`"), "hover should show Rust type: {hover}");
        assert!(hover.contains("CHECK (age >= 18)"), "hover should show SQL check: {hover}");

        let bio = ast
            .models()
            .flat_map(|(_, m)| m.fields())
            .find(|(_, f)| f.name == "bio")
            .map(|(_, f)| f.clone())
            .unwrap();
        let bio_hover = Backend::hover_for_field(&bio);
        assert!(bio_hover.contains("Option<String>"), "nullable -> Option: {bio_hover}");
    }

    #[test]
    fn definition_resolves_reference_to_model_line() {
        let src = "User\n  id uuid -pk\nPost\n  id uuid -pk\n  author User.id\n";
        let ast = parse(src).unwrap();
        let line = ast
            .models()
            .flat_map(|(_, m)| m.fields())
            .find(|(_, f)| f.name == "author")
            .map(|(_, f)| f.line)
            .unwrap();
        let field = field_at_line(&ast, line).unwrap();
        let target = Backend::referenced_model(&field);
        assert_eq!(target.as_deref(), Some("User"));
    }

    /// `AsyncWrite` sink that appends every written frame to a shared buffer.
    struct OutWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl futures::io::AsyncWrite for OutWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0.lock().unwrap().extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
}
