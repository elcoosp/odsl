//! The OSDL parser: pest token stream -> validated [`Ast`].
//!
//! The pipeline is: [`parse`] produces a raw, arena-populated [`Ast`] (with
//! unresolved references left as `FieldType::InferredRef` / word types), then
//! runs type inference so the returned AST is ready for the validator or
//! renderers (REQ-FUNC-002).

#![allow(clippy::result_large_err)]

pub mod infer;

use infer::infer_field_type;
use osdl_core::ast::{Ast, CustomType, Field, Model, ModelIndex};
use osdl_core::errors::{OsdlError, ParseError, Span};
use osdl_core::types::{FieldType, FkAction, Intent, Reference, ScalarType};
use pest::Parser;
use pest::iterators::{Pair, Pairs};
use std::collections::HashSet;

pub mod grammar {
    use pest_derive::Parser;
    #[derive(Parser)]
    #[grammar = "grammar.pest"]
    pub struct OsdlParser;
}

pub use grammar::Rule;

#[derive(Debug, Clone)]
enum RawToken {
    Name(String),
    Flag(String),
    Reference {
        model: String,
        field: String,
    },
    Word(String),
    /// A `use path::to::module` declaration (module import).
    Use(String),
    /// A `type Name = base -intents...` declaration (custom value object).
    /// `rhs` holds the parsed RHS tokens (base scalar + intents/check).
    TypeDecl {
        name: String,
        rhs: Vec<RawToken>,
    },
    /// A quoted string literal (`"..."`); the inner content (quotes stripped)
    /// is stored verbatim, e.g. `-default ""` yields `Quoted("")`.
    Quoted(String),
}

struct ParsedLine {
    indent: usize,
    tokens: Vec<RawToken>,
    line_no: usize,
    byte_start: usize,
    byte_end: usize,
    /// Doc-comment text from a `///` line, attached to the next declaration.
    doc: Option<String>,
}

/// Parse OSDL source into a resolved [`Ast`] (inference applied).
pub fn parse_and_resolve(src: &str) -> Result<Ast, OsdlError> {
    parse(src)
}

/// A single parsed source file: its AST plus the `use` declarations it makes.
pub struct FileAst {
    pub ast: Ast,
    /// Module paths referenced via `use` (e.g. `billing::invoice`).
    pub uses: Vec<String>,
}

/// Parse a single OSDL source file into an [`Ast`] (inference applied).
/// `use` declarations are recorded but not resolved (see [`parse_project`]).
pub fn parse_file(src: &str) -> Result<FileAst, OsdlError> {
    let pairs = grammar::OsdlParser::parse(Rule::file, src)
        .map_err(|e| OsdlError::Parse(ParseError::new(format!("parse error: {e}"))))?;

    let mut known_models: HashSet<String> = HashSet::new();
    let parsed = collect_lines(pairs, src)?;
    for line in &parsed {
        if line.indent == 0
            && !line.tokens.is_empty()
            && let Some(RawToken::Name(n)) = line.tokens.first()
        {
            known_models.insert(n.clone());
        }
    }

    // Pre-pass: collect custom type declarations (they may be referenced by
    // fields declared earlier in the file, so gather them up front).
    let mut custom_types: std::collections::HashMap<String, CustomType> = Default::default();
    for pl in &parsed {
        if pl.indent == 0 {
            if let Some(RawToken::TypeDecl { name, rhs }) = pl.tokens.first() {
                let ct = custom_type_from_tokens(
                    name,
                    rhs,
                    src,
                    pl.byte_start,
                    pl.byte_end,
                    pl.line_no,
                )?
                .ok_or_else(|| {
                    OsdlError::Parse(ParseError::new(format!(
                        "invalid custom type declaration for `{name}`"
                    )))
                })?;
                custom_types.insert(name.clone(), ct);
            }
        }
    }

    let mut ast = Ast::new();
    let mut current_model: Option<Model> = None;
    let mut uses: Vec<String> = Vec::new();
    // Doc comment (`///`) pending attachment to the next model/field declaration.
    let mut pending_doc: Option<String> = None;

    for pl in &parsed {
        // A `///` doc-comment line attaches to the next declaration. Consecutive
        // `///` lines accumulate into one multi-line doc. It carries no structural
        // tokens, so this check runs *before* the empty-token skip.
        if let Some(doc) = &pl.doc {
            pending_doc = Some(match pending_doc.take() {
                Some(prev) => format!("{prev}\n{doc}"),
                None => doc.clone(),
            });
            continue;
        }
        // Comment-only / blank lines carry no tokens — skip them.
        if pl.tokens.is_empty() {
            continue;
        }
        if pl.indent == 0 {
            // A `use` declaration is not a model.
            if let Some(RawToken::Use(path)) = pl.tokens.first() {
                uses.push(path.clone());
                continue;
            }
            // A `type` declaration is not a model.
            if let Some(RawToken::TypeDecl { name, .. }) = pl.tokens.first() {
                let ct = custom_types.get(name).cloned().expect("collected above");
                ast.add_custom_type(ct);
                continue;
            }
            if let Some(m) = current_model.take() {
                ast.add_model(m);
            }
            let name = match pl.tokens.first() {
                Some(RawToken::Name(n)) => n.clone(),
                _ => {
                    return Err(OsdlError::Parse(
                        ParseError::new("expected a model name at column 0")
                            .with_span(span_at(src, pl.byte_start, pl.byte_end, pl.line_no), src),
                    ));
                }
            };
            // Attach any pending doc comment to this model.
            if let Some(doc) = pending_doc.take() {
                ast.model_docs.insert(name.clone(), doc);
            }
            let mut model = Model {
                name,
                fields: la_arena::Arena::new(),
                field_index: vec![],
                line: pl.line_no,
                indexes: vec![],
            };
            // Model-level composite indexes: `-index a,b` / `-uniq a,b`
            // (may appear on the model-declaration line or as standalone lines).
            let model_tokens: Vec<_> = pl.tokens.iter().skip(1).cloned().collect();
            capture_model_index(&mut model, &model_tokens);

            let field_tokens: Vec<_> = model_tokens;
            if !field_tokens.is_empty() {
                add_field_from_tokens(
                    &mut model,
                    &field_tokens,
                    &known_models,
                    &custom_types,
                    pl.line_no,
                    pl.byte_start,
                    pl.byte_end,
                    src,
                )?;
            }
            current_model = Some(model);
        } else {
            let m =
                match current_model.as_mut() {
                    Some(m) => m,
                    None => return Err(OsdlError::Parse(
                        ParseError::new(
                            "field is not inside any model (indented line without a parent model)",
                        )
                        .with_span(span_at(src, pl.byte_start, pl.byte_end, pl.line_no), src),
                    )),
                };
            // A standalone indented line that begins with `-index`/`-uniq` is a
            // model-level composite-index directive, not a field.
            if capture_model_index(m, &pl.tokens) {
                continue;
            }
            // Record the (model, field) key for doc/deprecation attachment.
            let field_name = match pl.tokens.first() {
                Some(RawToken::Name(n)) | Some(RawToken::Word(n)) => n.clone(),
                _ => String::new(),
            };
            let (deprecated, _) = add_field_from_tokens(
                m,
                &pl.tokens,
                &known_models,
                &custom_types,
                pl.line_no,
                pl.byte_start,
                pl.byte_end,
                src,
            )?;
            // Attach any pending doc comment to this field.
            if let Some(doc) = pending_doc.take() {
                ast.field_docs
                    .insert((m.name.clone(), field_name.clone()), doc);
            }
            if let Some(reason) = deprecated {
                ast.field_deprecated
                    .insert((m.name.clone(), field_name), reason);
            }
        }
    }
    if let Some(m) = current_model.take() {
        ast.add_model(m);
    }

    Ok(FileAst { ast, uses })
}

/// Parse OSDL source into a resolved [`Ast`]. `use` declarations are parsed
/// but not resolved (single-file view). For multi-file module resolution use
/// [`parse_project`].
pub fn parse(src: &str) -> Result<Ast, OsdlError> {
    Ok(parse_file(src)?.ast)
}

/// The result of resolving a project: the merged [`Ast`] plus every source
/// file that contributed to it (entry first), in deterministic order. Used by
/// the lockfile to Merkle-hash all inputs.
#[derive(Debug)]
pub struct Project {
    pub ast: Ast,
    pub sources: Vec<std::path::PathBuf>,
}

/// Resolve a project rooted at `root` (an `.osdl` file). Recursively follows
/// `use` declarations, merging each imported file's models into a single AST.
///
/// Resolution rules:
/// * `use a::b::c` maps to `<root_dir>/a/b/c.osdl` (also tries `<root_dir>/a/b/c`
///   without extension and `<root_dir>/a/b/c/mod.osdl`).
/// * A model name may not collide across files (returns an error).
/// * Cycles are detected via a visited set.
pub fn parse_project(root: &std::path::Path) -> Result<Project, OsdlError> {
    let mut ast = Ast::new();
    let mut sources: Vec<std::path::PathBuf> = Vec::new();
    let mut visited: HashSet<String> = HashSet::new();
    resolve_file(root, &mut ast, &mut sources, &mut visited)?;
    Ok(Project { ast, sources })
}

fn resolve_file(
    path: &std::path::Path,
    ast: &mut Ast,
    sources: &mut Vec<std::path::PathBuf>,
    visited: &mut HashSet<String>,
) -> Result<(), OsdlError> {
    let canon = std::fs::canonicalize(path).map_err(|e| {
        OsdlError::Io(std::io::Error::other(format!(
            "reading {}: {e}",
            path.display()
        )))
    })?;
    let key = canon.to_string_lossy().to_string();
    if !visited.insert(key.clone()) {
        return Ok(()); // already merged (cycle guard)
    }

    let src = std::fs::read_to_string(&canon).map_err(|e| {
        OsdlError::Io(std::io::Error::other(format!(
            "reading {}: {e}",
            canon.display()
        )))
    })?;
    let file_ast = parse_file(&src)?;

    // Merge this file's models, detecting collisions.
    for (_, model) in file_ast.ast.models() {
        if ast.model_by_name(&model.name).is_some() {
            return Err(OsdlError::Parse(ParseError::new(format!(
                "duplicate model `{}` (imported via `use` from {})",
                model.name,
                canon.display()
            ))));
        }
        ast.add_model(model.clone());
    }
    sources.push(canon.clone());

    // Resolve imports relative to this file's directory.
    let dir = canon.parent().unwrap_or_else(|| std::path::Path::new("."));
    for use_path in &file_ast.uses {
        let target = resolve_use_path(dir, use_path);
        resolve_file(&target, ast, sources, visited)?;
    }
    Ok(())
}

/// Map `a::b::c` to a concrete `.osdl` path, trying a few conventions.
fn resolve_use_path(dir: &std::path::Path, use_path: &str) -> std::path::PathBuf {
    let rel = use_path.replace("::", "/");
    let base = dir.join(&rel);
    // Try, in order: <rel>.osdl, <rel>/mod.osdl, <rel> (bare).
    let candidates = [
        format!("{}.osdl", base.to_string_lossy()),
        format!("{}/mod.osdl", base.to_string_lossy()),
        base.to_string_lossy().to_string(),
    ];
    for c in &candidates {
        let p = std::path::PathBuf::from(c);
        if p.exists() {
            return p;
        }
    }
    // Default to the `.osdl` form even if it doesn't exist (error surfaces later).
    std::path::PathBuf::from(format!("{}.osdl", base.to_string_lossy()))
}

fn capture_model_index(model: &mut Model, tokens: &[RawToken]) -> bool {
    // A model-level composite-index directive: `-index a,b` / `-uniq a,b`.
    // Returns true if the tokens were consumed as such (so the caller should
    // not treat the line as a field).
    let (Some(RawToken::Flag(f)), Some(RawToken::Word(w))) = (tokens.first(), tokens.get(1)) else {
        return false;
    };
    let unique = match f.as_str() {
        "-uniq" | "-unique" => true,
        "-index" | "-idx" => false,
        _ => return false,
    };
    let fields: Vec<String> = w
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if fields.is_empty() {
        return false;
    }
    let idx_name = format!(
        "{}_{}",
        if unique { "uniq" } else { "idx" },
        fields.join("_")
    );
    model.indexes.push(ModelIndex {
        name: idx_name,
        fields,
        unique,
    });
    true
}

fn add_field_from_tokens(
    model: &mut Model,
    tokens: &[RawToken],
    known_models: &HashSet<String>,
    custom_types: &std::collections::HashMap<String, CustomType>,
    line_no: usize,
    byte_start: usize,
    byte_end: usize,
    src: &str,
) -> Result<(Option<String>, Option<String>), OsdlError> {
    let mut iter = tokens.iter().peekable();

    let name = match iter.peek() {
        Some(RawToken::Name(n)) | Some(RawToken::Word(n)) => {
            let n = n.clone();
            iter.next();
            n
        }
        _ => {
            return Err(OsdlError::Parse(
                ParseError::new("expected a field name")
                    .with_span(span_at(src, byte_start, byte_end, line_no), src),
            ));
        }
    };

    let mut ty: Option<FieldType> = None;
    let mut intents: Vec<Intent> = Vec::new();
    let mut enum_variants: Vec<String> = Vec::new();
    let mut default_value: Option<String> = None;
    let mut m2m_target: Option<String> = None;
    let mut check_expr: Option<String> = None;
    let mut polymorphic_targets: Vec<String> = Vec::new();
    let mut capturing_enum = false;
    let mut capturing_default = false;
    let mut capturing_m2m = false;
    let mut capturing_check = false;
    let mut capturing_polymorphic = false;
    let mut capturing_ondelete = false;
    let mut capturing_onupdate = false;
    let mut capturing_deprecated = false;
    let mut on_delete: Option<FkAction> = None;
    let mut on_update: Option<FkAction> = None;
    let mut deprecated: Option<String> = None;
    // If the field's type is a custom type, record its name and inherit its
    // base scalar + constraints.
    let mut custom_type: Option<String> = None;

    for tok in iter {
        match tok {
            RawToken::Flag(f) => {
                if f == "-enum" {
                    // The variants follow as a separate word token (e.g. `a,b`).
                    intents.push(Intent::Enum);
                    capturing_enum = true;
                    continue;
                }
                if f == "-default" {
                    // The value follows as a separate word token (e.g. `0`, `now`, `""`).
                    intents.push(Intent::Default);
                    capturing_default = true;
                    continue;
                }
                if f == "-m2m" || f == "-many" {
                    // The target model follows as a separate word/reference token.
                    intents.push(Intent::M2m);
                    capturing_m2m = true;
                    continue;
                }
                if f == "-check" {
                    // The boolean expression follows as a separate quoted token
                    // (e.g. `-check "age >= 18"`).
                    intents.push(Intent::Check);
                    capturing_check = true;
                    continue;
                }
                if f == "-polymorphic" {
                    // The target model list follows as a separate word token
                    // (e.g. `-polymorphic Post,Video`).
                    intents.push(Intent::Polymorphic);
                    capturing_polymorphic = true;
                    continue;
                }
                if f == "-ondelete" {
                    // The referential action follows as a separate word token
                    // (e.g. `-ondelete cascade`).
                    intents.push(Intent::OnDelete);
                    capturing_ondelete = true;
                    continue;
                }
                if f == "-onupdate" {
                    intents.push(Intent::OnUpdate);
                    capturing_onupdate = true;
                    continue;
                }
                if f == "-deprecated" {
                    // The reason follows as a separate quoted/word token
                    // (e.g. `-deprecated "use contactEmail instead"`).
                    capturing_deprecated = true;
                    continue;
                }
                let intent = parse_intent(f).ok_or_else(|| {
                    OsdlError::Parse(
                        ParseError::new(format!("unknown intent flag `{f}`"))
                            .with_span(span_at(src, byte_start, byte_end, line_no), src),
                    )
                })?;
                intents.push(intent);
            }
            RawToken::Reference {
                model: rm,
                field: rf,
            } => {
                if capturing_m2m {
                    capturing_m2m = false;
                    m2m_target = Some(rm.clone());
                    continue;
                }
                ty = Some(FieldType::Ref(Reference {
                    model: rm.clone(),
                    field: rf.clone(),
                }));
            }
            RawToken::Word(w) => {
                if capturing_enum {
                    capturing_enum = false;
                    enum_variants = w.split(',').map(|s| s.trim().to_string()).collect();
                    continue;
                }
                if capturing_default {
                    capturing_default = false;
                    default_value = Some(w.trim().to_string());
                    continue;
                }
                if capturing_check {
                    capturing_check = false;
                    check_expr = Some(w.trim().to_string());
                    continue;
                }
                if capturing_polymorphic {
                    capturing_polymorphic = false;
                    polymorphic_targets = w
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                    continue;
                }
                if capturing_ondelete {
                    capturing_ondelete = false;
                    on_delete = Some(
                        FkAction::from_keyword(w.trim()).ok_or_else(|| {
                            OsdlError::Parse(
                                ParseError::new(format!(
                                    "invalid -ondelete action `{w}` (expected cascade|restrict|setnull|setdefault|noaction)"
                                ))
                                .with_span(span_at(src, byte_start, byte_end, line_no), src),
                            )
                        })?,
                    );
                    continue;
                }
                if capturing_onupdate {
                    capturing_onupdate = false;
                    on_update = Some(
                        FkAction::from_keyword(w.trim()).ok_or_else(|| {
                            OsdlError::Parse(
                                ParseError::new(format!(
                                    "invalid -onupdate action `{w}` (expected cascade|restrict|setnull|setdefault|noaction)"
                                ))
                                .with_span(span_at(src, byte_start, byte_end, line_no), src),
                            )
                        })?,
                    );
                    continue;
                }
                if capturing_deprecated {
                    capturing_deprecated = false;
                    deprecated = Some(w.trim().to_string());
                    continue;
                }
                if capturing_m2m {
                    capturing_m2m = false;
                    m2m_target = Some(w.trim().to_string());
                    continue;
                }
                if let Some(ct) = custom_types.get(w) {
                    // This field is declared with a custom type: expand to its
                    // base scalar and inherit its intents/check. The field
                    // remains a normal scalar but carries the custom-type name.
                    custom_type = Some(ct.name.clone());
                    ty = Some(FieldType::Scalar(ct.base));
                    for intent in &ct.intents {
                        if !intents.contains(intent) {
                            intents.push(*intent);
                        }
                    }
                    if ct.enum_variants.is_empty() {
                        enum_variants = ct.enum_variants.clone();
                    }
                    if ct.default_value.is_some() {
                        default_value = ct.default_value.clone();
                    }
                    if ct.check_expr.is_some() {
                        check_expr = ct.check_expr.clone();
                    }
                    continue;
                }
                if let Some(target) = w.strip_prefix("relation:") {
                    intents.push(Intent::Relation);
                    ty = Some(FieldType::InferredRef(format!("relation:{target}")));
                } else if let Some(s) = ScalarType::from_keyword(w) {
                    ty = Some(FieldType::Scalar(s));
                } else if known_models.contains(w) {
                    ty = Some(FieldType::Ref(Reference {
                        model: w.clone(),
                        field: "id".into(),
                    }));
                } else {
                    ty = Some(FieldType::InferredRef(w.clone()));
                }
            }
            RawToken::Quoted(q) => {
                if capturing_default {
                    capturing_default = false;
                    default_value = Some(q.clone());
                    continue;
                }
                if capturing_check {
                    capturing_check = false;
                    check_expr = Some(q.clone());
                    continue;
                }
                if capturing_deprecated {
                    capturing_deprecated = false;
                    deprecated = Some(q.clone());
                    continue;
                }
                // A bare quoted string as a type/ref token is unsupported;
                // treat it as an inferred reference name (best-effort).
                ty = Some(FieldType::InferredRef(q.clone()));
            }
            RawToken::Name(_) => unreachable!("name only appears as first token"),
            RawToken::Use(_) => unreachable!("use only appears at indent 0, not as a field"),
            RawToken::TypeDecl { .. } => {
                unreachable!("type only appears at indent 0, not as a field")
            }
        }
    }

    let ty = match ty {
        Some(t) => t,
        None => infer_field_type(&name, known_models, &model.name),
    };

    model.add_field(Field {
        name,
        ty,
        intents,
        enum_variants,
        default_value,
        m2m_target,
        check_expr,
        polymorphic_targets,
        custom_type,
        on_delete,
        on_update,
        line: line_no,
    });
    Ok((deprecated, None))
}

/// Build a [`CustomType`] from the RHS tokens of a `type X = ...` declaration.
/// Returns `None` if the RHS has no base scalar (caller turns that into an error).
fn custom_type_from_tokens(
    name: &str,
    rhs: &[RawToken],
    src: &str,
    byte_start: usize,
    byte_end: usize,
    line_no: usize,
) -> Result<Option<CustomType>, OsdlError> {
    let mut base: Option<ScalarType> = None;
    let mut intents: Vec<Intent> = Vec::new();
    let mut enum_variants: Vec<String> = Vec::new();
    let mut default_value: Option<String> = None;
    let mut check_expr: Option<String> = None;
    let mut capturing_enum = false;
    let mut capturing_default = false;
    let mut capturing_check = false;

    for tok in rhs {
        match tok {
            RawToken::Flag(f) => {
                if f == "-enum" {
                    intents.push(Intent::Enum);
                    capturing_enum = true;
                    continue;
                }
                if f == "-default" {
                    intents.push(Intent::Default);
                    capturing_default = true;
                    continue;
                }
                if f == "-check" {
                    intents.push(Intent::Check);
                    capturing_check = true;
                    continue;
                }
                let intent = parse_intent(f).ok_or_else(|| {
                    OsdlError::Parse(
                        ParseError::new(format!("unknown intent flag `{f}` in type `{name}`"))
                            .with_span(span_at(src, byte_start, byte_end, line_no), src),
                    )
                })?;
                intents.push(intent);
            }
            RawToken::Word(w) => {
                if capturing_enum {
                    capturing_enum = false;
                    enum_variants = w.split(',').map(|s| s.trim().to_string()).collect();
                    continue;
                }
                if capturing_default {
                    capturing_default = false;
                    default_value = Some(w.trim().to_string());
                    continue;
                }
                if capturing_check {
                    capturing_check = false;
                    check_expr = Some(w.trim().to_string());
                    continue;
                }
                if let Some(s) = ScalarType::from_keyword(w) {
                    base = Some(s);
                } else {
                    return Err(OsdlError::Parse(
                        ParseError::new(format!(
                            "custom type `{name}` must be based on a scalar, got `{w}`"
                        ))
                        .with_span(span_at(src, byte_start, byte_end, line_no), src),
                    ));
                }
            }
            RawToken::Quoted(q) => {
                if capturing_default {
                    capturing_default = false;
                    default_value = Some(q.clone());
                    continue;
                }
                if capturing_check {
                    capturing_check = false;
                    check_expr = Some(q.clone());
                    continue;
                }
                base = None; // invalid; error below
            }
            _ => {}
        }
    }

    let Some(base) = base else {
        return Ok(None);
    };
    Ok(Some(CustomType {
        name: name.to_string(),
        base,
        intents,
        enum_variants,
        default_value,
        check_expr,
    }))
}

fn parse_intent(flag: &str) -> Option<Intent> {
    match flag {
        "-pk" | "-primary" => Some(Intent::Pk),
        "-partition" => Some(Intent::Partition),
        "-uniq" | "-unique" => Some(Intent::Uniq),
        "-null" | "-nullable" => Some(Intent::Null),
        "-fulltext" => Some(Intent::Fulltext),
        "-index" | "-idx" => Some(Intent::Index),
        "-tz" | "-timezone" => Some(Intent::Tz),
        "-auto" | "-autoincrement" => Some(Intent::Auto),
        "-relation" => Some(Intent::Relation),
        "-enum" => Some(Intent::Enum),
        "-default" => Some(Intent::Default),
        "-m2m" | "-many" => Some(Intent::M2m),
        "-virtual" => Some(Intent::Virtual),
        "-softdelete" => Some(Intent::SoftDelete),
        "-check" => Some(Intent::Check),
        "-polymorphic" => Some(Intent::Polymorphic),
        _ => None,
    }
}

fn collect_lines(pairs: Pairs<Rule>, src: &str) -> Result<Vec<ParsedLine>, OsdlError> {
    let mut lines = Vec::new();
    // `parse` yields a single top-level `file` pair; descend into it.
    for pair in pairs {
        if pair.as_rule() == Rule::file {
            for inner in pair.into_inner() {
                match inner.as_rule() {
                    Rule::line => lines.push(parse_line(inner, src)?),
                    Rule::EOI => {}
                    _ => {}
                }
            }
        } else if pair.as_rule() == Rule::line {
            lines.push(parse_line(pair, src)?);
        }
    }
    Ok(lines)
}

fn parse_line(pair: Pair<Rule>, src: &str) -> Result<ParsedLine, OsdlError> {
    let line_no = pair.line_col().0;
    let span = pair.as_span();
    let byte_start = span.start();
    let byte_end = span.end();

    let mut indent = 0usize;
    let mut tokens = Vec::new();
    let mut doc_accum: Option<String> = None;
    for inner in pair.into_inner() {
        match inner.as_rule() {
            Rule::indent => indent = inner.as_str().chars().count(),
            Rule::body => {
                // Descend into the body to collect name/token/comment.
                for b in inner.into_inner() {
                    match b.as_rule() {
                        Rule::name => tokens.push(RawToken::Name(b.as_str().to_string())),
                        Rule::token => {
                            let t =
                                b.into_inner().next().ok_or_else(|| {
                                    OsdlError::Parse(ParseError::new("empty token").with_span(
                                        span_at(src, byte_start, byte_end, line_no),
                                        src,
                                    ))
                                })?;
                            match t.as_rule() {
                                Rule::flag => tokens.push(RawToken::Flag(t.as_str().to_string())),
                                Rule::reference => {
                                    let (m, f) = split_reference(t.as_str());
                                    tokens.push(RawToken::Reference { model: m, field: f });
                                }
                                Rule::quoted => {
                                    let s = t.as_str();
                                    // Strip the surrounding double quotes.
                                    let inner = if s.len() >= 2 { &s[1..s.len() - 1] } else { "" };
                                    tokens.push(RawToken::Quoted(inner.to_string()));
                                }
                                Rule::word => tokens.push(RawToken::Word(t.as_str().to_string())),
                                _ => {}
                            }
                        }
                        Rule::comment_part => { /* comments carry no tokens */ }
                        Rule::doc_comment => {
                            // Strip the leading `///` and surrounding whitespace.
                            let raw = b.as_str();
                            let text = raw.trim_start_matches("///").trim().to_string();
                            // Accumulate into the line's doc (join multiple `///`).
                            if let Some(existing) = &mut doc_accum {
                                existing.push('\n');
                                existing.push_str(&text);
                            } else {
                                doc_accum = Some(text);
                            }
                        }
                        Rule::use_stmt => {
                            // Extract the module path from the `use_kw ~ sp ~ module_path`.
                            let mut path = String::new();
                            for u in b.into_inner() {
                                if u.as_rule() == Rule::module_path {
                                    path = u.as_str().to_string();
                                }
                            }
                            tokens.push(RawToken::Use(path));
                        }
                        Rule::type_decl => {
                            // `type_kw ~ sp ~ ident ~ sp ~ equals ~ sp ~ type_rhs`
                            let mut name = String::new();
                            let mut rhs: Vec<RawToken> = Vec::new();
                            for u in b.into_inner() {
                                match u.as_rule() {
                                    Rule::ident => name = u.as_str().to_string(),
                                    Rule::type_rhs => {
                                        for t in u.into_inner() {
                                            match t.as_rule() {
                                                Rule::word => {
                                                    rhs.push(RawToken::Word(t.as_str().to_string()))
                                                }
                                                Rule::token => {
                                                    let inner = t.into_inner().next().unwrap();
                                                    match inner.as_rule() {
                                                        Rule::flag => rhs.push(RawToken::Flag(
                                                            inner.as_str().to_string(),
                                                        )),
                                                        Rule::reference => {
                                                            let (m, f) =
                                                                split_reference(inner.as_str());
                                                            rhs.push(RawToken::Reference {
                                                                model: m,
                                                                field: f,
                                                            });
                                                        }
                                                        Rule::quoted => {
                                                            let s = inner.as_str();
                                                            let q = if s.len() >= 2 {
                                                                &s[1..s.len() - 1]
                                                            } else {
                                                                ""
                                                            };
                                                            rhs.push(RawToken::Quoted(
                                                                q.to_string(),
                                                            ));
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            tokens.push(RawToken::TypeDecl { name, rhs });
                        }
                        _ => {}
                    }
                }
            }
            Rule::newline => {}
            _ => {}
        }
    }
    Ok(ParsedLine {
        indent,
        tokens,
        line_no,
        byte_start,
        byte_end,
        doc: doc_accum,
    })
}

fn split_reference(s: &str) -> (String, String) {
    match s.split_once('.') {
        Some((m, f)) => (m.to_string(), f.to_string()),
        None => (s.to_string(), "id".into()),
    }
}

fn span_at(_src: &str, start: usize, end: usize, line: usize) -> Span {
    Span {
        start,
        end,
        line,
        column: 1,
    }
}

#[cfg(test)]
mod enum_tests {
    use super::*;
    use osdl_core::types::Intent;

    #[test]
    fn parses_default_value() {
        let src = "User\n  id uuid -pk\n  age int -default 0\n  created datetime -default now\n  bio string -default \"\"\n";
        let ast = parse(src).unwrap();
        let midx = ast.model_by_name("User").unwrap();
        let m = &ast.models[midx];
        let age = m.fields().find(|(_, fl)| fl.name == "age").unwrap().1;
        assert!(age.has(Intent::Default));
        assert_eq!(age.default_value.as_deref(), Some("0"));
        let created = m.fields().find(|(_, fl)| fl.name == "created").unwrap().1;
        assert_eq!(created.default_value.as_deref(), Some("now"));
        let bio = m.fields().find(|(_, fl)| fl.name == "bio").unwrap().1;
        assert_eq!(bio.default_value.as_deref(), Some(""));
    }

    #[test]
    fn parses_composite_model_indexes() {
        let src = "User\n  id uuid -pk\n  tenant_id uuid\n  email string\n  -uniq tenant_id,email\n  -index tenant_id,created_at\n";
        let ast = parse(src).unwrap();
        let midx = ast.model_by_name("User").unwrap();
        let m = &ast.models[midx];
        let uniq = m.indexes.iter().find(|i| i.unique).unwrap();
        assert_eq!(
            uniq.fields,
            vec!["tenant_id".to_string(), "email".to_string()]
        );
        assert_eq!(uniq.name, "uniq_tenant_id_email");
        let idx = m.indexes.iter().find(|i| !i.unique).unwrap();
        assert_eq!(
            idx.fields,
            vec!["tenant_id".to_string(), "created_at".to_string()]
        );
    }

    #[test]
    fn parses_many_to_many() {
        let src = "User\n  id uuid -pk\n  posts -m2m Post\nPost\n  id uuid -pk\n";
        let ast = parse(src).unwrap();
        let midx = ast.model_by_name("User").unwrap();
        let m = &ast.models[midx];
        let f = m.fields().find(|(_, fl)| fl.name == "posts").unwrap().1;
        assert!(f.has(Intent::M2m));
        assert_eq!(f.m2m_target.as_deref(), Some("Post"));
    }

    #[test]
    fn parses_enum_variants() {
        let src = "User\n  id uuid -pk\n  status string -enum active,inactive,pending\n";
        let ast = parse(src).unwrap();
        let midx = ast.model_by_name("User").unwrap();
        let m = &ast.models[midx];
        let f = m.fields().find(|(_, fl)| fl.name == "status").unwrap().1;
        assert!(f.has(Intent::Enum));
        assert_eq!(
            f.enum_variants,
            vec![
                "active".to_string(),
                "inactive".to_string(),
                "pending".to_string()
            ]
        );
    }

    #[test]
    fn parses_virtual_field() {
        let src = "User\n  id uuid -pk\n  display_name string -virtual\n";
        let ast = parse(src).unwrap();
        let f = ast
            .models()
            .flat_map(|(_, m)| m.fields())
            .find(|(_, fl)| fl.name == "display_name")
            .unwrap()
            .1;
        assert!(f.has(Intent::Virtual));
    }

    #[test]
    fn parses_softdelete_field() {
        let src = "User\n  id uuid -pk\n  deleted_at datetime -null -softdelete\n";
        let ast = parse(src).unwrap();
        let f = ast
            .models()
            .flat_map(|(_, m)| m.fields())
            .find(|(_, fl)| fl.name == "deleted_at")
            .unwrap()
            .1;
        assert!(f.has(Intent::SoftDelete));
        assert!(f.has(Intent::Null));
    }

    #[test]
    fn parses_check_constraint() {
        let src = "User\n  id uuid -pk\n  age int -check \"age >= 18\"\n";
        let ast = parse(src).unwrap();
        let f = ast
            .models()
            .flat_map(|(_, m)| m.fields())
            .find(|(_, fl)| fl.name == "age")
            .unwrap()
            .1;
        assert!(f.has(Intent::Check));
        assert_eq!(f.check_expr.as_deref(), Some("age >= 18"));
    }

    #[test]
    fn parses_polymorphic_reference() {
        let src = "Comment\n  id uuid -pk\n  target -polymorphic Post,Video\nPost\n  id uuid -pk\nVideo\n  id uuid -pk\n";
        let ast = parse(src).unwrap();
        let f = ast
            .models()
            .flat_map(|(_, m)| m.fields())
            .find(|(_, fl)| fl.name == "target")
            .unwrap()
            .1;
        assert!(f.has(Intent::Polymorphic));
        assert_eq!(
            f.polymorphic_targets,
            vec!["Post".to_string(), "Video".to_string()]
        );
    }
}
