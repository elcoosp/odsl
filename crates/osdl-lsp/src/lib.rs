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
    self, Diagnostic, DidChangeTextDocumentParams, DidOpenTextDocumentParams, InitializeParams,
    InitializeResult, PublishDiagnosticsParams, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind,
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
