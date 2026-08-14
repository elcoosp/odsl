//! # OSDL Core — the shared contract of the OSDL compiler.
//!
//! This crate defines the vocabulary every other workspace member speaks:
//! the AST, the primitive/intent types, validation rules, the
//! [`CodeRenderer`] trait, the target enumeration and the lockfile format.
//! It deliberately depends on no parsing or codegen logic.

#![allow(clippy::result_large_err)]

pub mod ast;
pub mod errors;
pub mod intent_compat;
pub mod lockfile;
pub mod types;
pub mod validator;

pub use ast::{Ast, Field, FieldIdx, LockField, LockModel, Model, ModelIdx};
pub use errors::{CompileErrorKind, OsdlError, ParseError, Span};
pub use intent_compat::Target;
pub use lockfile::Lockfile;
pub use types::{FieldType, Intent, Reference, ScalarType};
pub use validator::{CodeRenderer, Validator};

/// The conventional name of an OSDL schema file.
pub const SCHEMA_FILE: &str = "schema.osdl";
/// The conventional name of the generated lockfile.
pub const LOCKFILE: &str = "osdl.lock";
/// The conventional output directory for generated entities.
pub const ENTITY_DIR: &str = "src/entity";
