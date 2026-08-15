//! Error and diagnostic types for the OSDL compiler.
//!
//! All errors implement [`miette::Diagnostic`] so the CLI can render
//! compiler-grade, colored messages that point at exact source spans. The
//! `Diagnostic` impls are written by hand (rather than via `#[derive]`) because
//! our [`Span`] is a custom type and we want precise control over labels and
//! source-code attachment.

use miette::{Diagnostic, LabeledSpan, SourceCode};
use std::fmt;
use thiserror::Error;

/// Source span within a `.osdl` file (0-indexed byte offsets).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}

/// A parse/lex error with precise source location.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct ParseError {
    pub message: String,
    /// The offending source region, if known.
    pub span: Option<Span>,
    /// The full `.osdl` source, used to render the label.
    pub src: Option<String>,
}

impl ParseError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            src: None,
        }
    }

    pub fn with_span(mut self, span: Span, src: &str) -> Self {
        self.span = Some(span);
        self.src = Some(src.to_string());
        self
    }
}

impl Diagnostic for ParseError {
    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        let span = self.span?;
        let len = span.end.saturating_sub(span.start);
        let label = LabeledSpan::new(Some("parse error".into()), span.start, len);
        Some(Box::new(std::iter::once(label)))
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        self.src.as_ref().map(|s| s as &dyn SourceCode)
    }
}

/// A logical compiler error raised during validation (references, business
/// rules, target compatibility, ...). These carry enough structured
/// information for precise diagnostics and for the BDD acceptance scenarios.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileErrorKind {
    /// BR-002 / REQ-FUNC-004: reference target does not exist.
    UnresolvedReference { from: String, target: String },
    /// REQ-FUNC-006: cyclic dependency between models.
    CyclicDependency { models: Vec<String> },
    /// REQ-FUNC-007: intent flag incompatible with field type.
    TypeMismatch { intent: String, ty: String },
    /// REQ-FUNC-008 / BR-003: target DB lacks a required intent.
    TargetIncompatibility {
        feature: String,
        target: String,
        detail: String,
    },
    /// BR-001: a model has no primary/partition key.
    MissingKey { model: String },
    /// A target was requested that is unknown.
    UnknownTarget { target: String },
}

impl fmt::Display for CompileErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileErrorKind::UnresolvedReference { from, target } => {
                write!(
                    f,
                    "Unresolved reference: `{from}` points to unknown {target}"
                )
            }
            CompileErrorKind::CyclicDependency { models } => {
                write!(
                    f,
                    "Cyclic Dependency Detected between {}",
                    models.join(" and ")
                )
            }
            CompileErrorKind::TypeMismatch { intent, ty } => {
                write!(f, "Type Mismatch: {intent} cannot be applied to {ty}")
            }
            CompileErrorKind::TargetIncompatibility {
                feature,
                target,
                detail,
            } => {
                write!(
                    f,
                    "Target Incompatibility: {target} does not support {feature} natively ({detail})"
                )
            }
            CompileErrorKind::MissingKey { model } => {
                write!(
                    f,
                    "Model `{model}` must declare exactly one primary (-pk) or partition (-partition) key"
                )
            }
            CompileErrorKind::UnknownTarget { target } => {
                write!(f, "Unknown target `{target}`")
            }
        }
    }
}

/// The top-level error returned by the compiler pipeline.
#[derive(Debug, Error)]
pub enum OsdlError {
    #[error(transparent)]
    Parse(#[from] ParseError),

    #[error("{kind}")]
    Compile {
        kind: CompileErrorKind,
        span: Option<Span>,
        src: Option<String>,
    },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl OsdlError {
    pub fn compile(kind: CompileErrorKind) -> Self {
        OsdlError::Compile {
            kind,
            span: None,
            src: None,
        }
    }

    pub fn compile_spanned(kind: CompileErrorKind, span: Span, src: &str) -> Self {
        OsdlError::Compile {
            kind,
            span: Some(span),
            src: Some(src.to_string()),
        }
    }
}

impl Diagnostic for OsdlError {
    fn labels(&self) -> Option<Box<dyn Iterator<Item = LabeledSpan> + '_>> {
        match self {
            OsdlError::Parse(p) => p.labels(),
            OsdlError::Compile {
                span: Some(s),
                kind,
                ..
            } => {
                let len = s.end.saturating_sub(s.start);
                let label = LabeledSpan::new(Some(kind.to_string()), s.start, len);
                Some(Box::new(std::iter::once(label)))
            }
            _ => None,
        }
    }

    fn source_code(&self) -> Option<&dyn SourceCode> {
        match self {
            OsdlError::Parse(p) => p.source_code(),
            OsdlError::Compile { src: Some(s), .. } => Some(s as &dyn SourceCode),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_from_converts_with_chain() {
        // std::io::Error must convert via #[from] and remain a miette Diagnostic.
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "boom");
        let e: OsdlError = io_err.into();
        assert!(matches!(e, OsdlError::Io(_)));
        // miette chaining: the inner io error should be reachable via source().
        let inner = std::error::Error::source(&e);
        assert!(
            inner.is_some(),
            "miette should chain the underlying io error"
        );
    }

    #[test]
    fn json_from_converts() {
        let bad = "not json";
        let json_err: serde_json::Error =
            serde_json::from_str::<serde_json::Value>(bad).unwrap_err();
        let e: OsdlError = json_err.into();
        assert!(matches!(e, OsdlError::Json(_)));
    }
}
