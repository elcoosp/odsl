//! The OSDL parser: pest token stream -> validated [`Ast`].
//!
//! The pipeline is: [`parse`] produces a raw, arena-populated [`Ast`] (with
//! unresolved references left as `FieldType::InferredRef` / word types), then
//! runs type inference so the returned AST is ready for the validator or
//! renderers (REQ-FUNC-002).

#![allow(clippy::result_large_err)]

pub mod infer;

use infer::infer_field_type;
use osdl_core::ast::{
    Ast, CustomType, Field, Model, ModelIndex, SchemaConfig, Seed, SeedRow, View, ViewField,
};
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
    /// Original source line text (used to extract verbatim view-query bodies).
    raw: String,
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
            && let Some(RawToken::Name(n) | RawToken::Word(n)) = line.tokens.first()
        {
            known_models.insert(n.clone());
        }
    }

    // Pre-pass: collect custom type declarations (they may be referenced by
    // fields declared earlier in the file, so gather them up front).
    let mut custom_types: std::collections::HashMap<String, CustomType> = Default::default();
    for pl in &parsed {
        if pl.indent == 0
            && let Some(RawToken::TypeDecl { name, rhs }) = pl.tokens.first()
        {
            let ct =
                custom_type_from_tokens(name, rhs, src, pl.byte_start, pl.byte_end, pl.line_no)?
                    .ok_or_else(|| {
                        OsdlError::Parse(ParseError::new(format!(
                            "invalid custom type declaration for `{name}`"
                        )))
                    })?;
            custom_types.insert(name.clone(), ct);
        }
    }

    let mut ast = Ast::new();
    let mut current_model: Option<Model> = None;
    let mut uses: Vec<String> = Vec::new();
    // Top-level `config` block accumulation (roadmap Phase 1.4).
    let mut parsing_config = false;
    let mut config = SchemaConfig::default();
    // Top-level `view` block accumulation (roadmap Phase 1.5). A view's query
    // body may span multiple physical lines; `current_view` holds the
    // in-progress view and `parsing_view` marks that subsequent indented lines
    // are query continuations (not fields/models).
    let mut parsing_view = false;
    let mut current_view: Option<View> = None;
    // Top-level `seed` block accumulation (roadmap Phase 1.6). A seed's rows
    // live on following indented continuation lines; `current_seed` holds the
    // in-progress seed and `parsing_seed` marks that subsequent indented lines
    // are seed rows (not fields/models).
    let mut parsing_seed = false;
    let mut current_seed: Option<Seed> = None;
    // Doc comment (`///`) pending attachment to the next model/field declaration.
    let mut pending_doc: Option<String> = None;

    for pl in &parsed {
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
        // While parsing a `seed`, indented lines are seed rows. A new
        // top-level (indent 0) declaration ends the seed and is handled by the
        // normal logic below in the *same* iteration.
        if parsing_seed {
            if pl.indent == 0 {
                if let Some(s) = current_seed.take() {
                    ast.add_seed(s);
                }
                parsing_seed = false;
                // Fall through to handle `pl` as a normal top-level declaration.
            } else {
                if let Some(s) = current_seed.as_mut() {
                    let row = parse_seed_row(&pl.raw, pl.line_no)?;
                    s.rows.push(row);
                }
                continue;
            }
        }
        // While parsing a `view`, indented lines are query continuations. A new
        // top-level (indent 0) declaration ends the view and is handled by the
        // normal logic below in the *same* iteration.
        if parsing_view {
            if pl.indent == 0 {
                if let Some(v) = current_view.take() {
                    ast.add_view(v);
                }
                parsing_view = false;
                // Fall through to handle `pl` as a normal top-level declaration.
            } else {
                if let Some(v) = current_view.as_mut() {
                    let chunk = pl.raw.trim();
                    if !v.query.is_empty() {
                        v.query.push(' ');
                    }
                    v.query.push_str(chunk);
                }
                continue;
            }
        }
        if pl.indent == 0 {
            // A `config` block (roadmap Phase 1.4): top-level `config` begins a
            // block of indented settings. We don't create a model for it.
            if let Some(RawToken::Name(n) | RawToken::Word(n)) = pl.tokens.first()
                && n == "config"
            {
                if let Some(m) = current_model.take() {
                    ast.add_model(m);
                }
                parsing_config = true;
                // Settings may also appear on the `config` line itself.
                let cfg_tokens: Vec<_> = pl.tokens.iter().skip(1).cloned().collect();
                if !cfg_tokens.is_empty() {
                    apply_config_line(&mut config, &cfg_tokens);
                }
                continue;
            }
            // A `view` declaration (roadmap Phase 1.5): `view Name [field type,
            // ...] [-materialized] = SELECT ...`. The query body may continue on
            // following indented lines.
            if let Some(RawToken::Name(n) | RawToken::Word(n)) = pl.tokens.first()
                && n == "view"
            {
                if let Some(m) = current_model.take() {
                    ast.add_model(m);
                }
                let view = parse_view_decl(&pl.tokens, pl.line_no, &pl.raw)?;
                current_view = Some(view);
                parsing_view = true;
                continue;
            }
            // A `seed` declaration (roadmap Phase 1.6): `seed Model`. The rows
            // follow on indented continuation lines.
            if let Some(RawToken::Name(n) | RawToken::Word(n)) = pl.tokens.first()
                && n == "seed"
            {
                if let Some(m) = current_model.take() {
                    ast.add_model(m);
                }
                let model = pl
                    .tokens
                    .get(1)
                    .map(|t| match t {
                        RawToken::Name(m) | RawToken::Word(m) => m.clone(),
                        _ => String::new(),
                    })
                    .unwrap_or_default();
                let seed = Seed {
                    model,
                    rows: vec![],
                    line: pl.line_no,
                };
                current_seed = Some(seed);
                parsing_seed = true;
                continue;
            }
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
            // Leaving a `config` block: its settings are already accumulated in
            // the local `config` and will be stored on `ast` at loop end.
            parsing_config = false;
            if let Some(m) = current_model.take() {
                ast.add_model(m);
            }
            let name = match pl.tokens.first() {
                Some(RawToken::Name(n)) | Some(RawToken::Word(n)) => n.clone(),
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
                primary_key: vec![],
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
                    &FieldEnv {
                        known_models: &known_models,
                        custom_types: &custom_types,
                    },
                    pl.line_no,
                    pl.byte_start,
                    pl.byte_end,
                    src,
                )?;
            }
            current_model = Some(model);
        } else {
            // Indented lines inside a `config` block are settings, not fields.
            if parsing_config {
                apply_config_line(&mut config, &pl.tokens);
                continue;
            }
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
            // A standalone indented line `-pk a,b` declares a composite primary
            // key at the model level.
            if capture_model_pk(m, &pl.tokens) {
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
                &FieldEnv {
                    known_models: &known_models,
                    custom_types: &custom_types,
                },
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
    // Finalize any view still open at EOF (e.g. a view whose last continuation
    // line is the final line of the file).
    if let Some(v) = current_view.take() {
        ast.add_view(v);
    }
    // Finalize any seed still open at EOF.
    if let Some(s) = current_seed.take() {
        ast.add_seed(s);
    }
    // Store the accumulated `config` block on the AST.
    ast.config = config;

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

    // Merge views (roadmap Phase 1.5). A view name must be unique across the
    // whole project.
    for v in file_ast.ast.views() {
        if ast.view_by_name(&v.name).is_some() {
            return Err(OsdlError::Parse(ParseError::new(format!(
                "duplicate view `{}` (imported via `use` from {})",
                v.name,
                canon.display()
            ))));
        }
        ast.add_view(v.clone());
    }

    // Merge seeds (roadmap Phase 1.6). A seed's target model must be unique
    // across the whole project.
    for s in file_ast.ast.seeds() {
        if ast.seed_by_name(&s.model).is_some() {
            return Err(OsdlError::Parse(ParseError::new(format!(
                "duplicate seed for model `{}` (imported via `use` from {})",
                s.model,
                canon.display()
            ))));
        }
        ast.add_seed(s.clone());
    }

    // Merge custom types. A name collision across files is an error.
    for (name, ct) in &file_ast.ast.custom_types {
        if ast.custom_type_by_name(name).is_some() {
            return Err(OsdlError::Parse(ParseError::new(format!(
                "duplicate custom type `{name}` (imported via `use` from {})",
                canon.display()
            ))));
        }
        ast.add_custom_type(ct.clone());
    }

    // Merge doc / deprecation metadata. The same (model,field) key must not be
    // declared in two files; if it is, the later one wins only when it differs
    // is not desirable — surface a collision to keep the merge deterministic.
    for (key, val) in &file_ast.ast.model_docs {
        if let Some(existing) = ast.model_docs.get(key) {
            if existing != val {
                return Err(OsdlError::Parse(ParseError::new(format!(
                    "duplicate doc-comment for model `{key}` (imported via `use` from {})",
                    canon.display()
                ))));
            }
        } else {
            ast.model_docs.insert(key.clone(), val.clone());
        }
    }
    for (key, val) in &file_ast.ast.field_docs {
        if let Some(existing) = ast.field_docs.get(key) {
            if existing != val {
                return Err(OsdlError::Parse(ParseError::new(format!(
                    "duplicate doc-comment for field `{}.{}` (imported via `use` from {})",
                    key.0, key.1,
                    canon.display()
                ))));
            }
        } else {
            ast.field_docs.insert(key.clone(), val.clone());
        }
    }
    for (key, val) in &file_ast.ast.field_deprecated {
        if let Some(existing) = ast.field_deprecated.get(key) {
            if existing != val {
                return Err(OsdlError::Parse(ParseError::new(format!(
                    "duplicate deprecation for field `{}.{}` (imported via `use` from {})",
                    key.0, key.1,
                    canon.display()
                ))));
            }
        } else {
            ast.field_deprecated.insert(key.clone(), val.clone());
        }
    }

    // Merge the schema config block. The entry file's config wins if set; an
    // imported file may only refine keys the entry left unset.
    ast.config.merge_from(&file_ast.ast.config)?;

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
    // A model-level composite-index directive: `-index a,b` / `-uniq a,b`,
    // optionally followed by index option flags:
    //   -type gin|gist|btree|hash   (index method)
    //   -prefix 10                  (MySQL prefix length on first column)
    //   -where "deleted_at IS NULL"  (partial-index predicate)
    //   -order desc                 (sort order on first column)
    //   -nulls first|last           (NULLS placement, Postgres)
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
    // Parse trailing option flags.
    let mut index_type: Option<String> = None;
    let mut prefix_length: Option<u16> = None;
    let mut where_clause: Option<String> = None;
    let mut order: Option<String> = None;
    let mut nulls: Option<String> = None;
    let mut i = 2;
    while i < tokens.len() {
        if let RawToken::Flag(flag) = &tokens[i] {
            match flag.as_str() {
                "-type" => {
                    if let Some(RawToken::Word(v)) = tokens.get(i + 1) {
                        index_type = Some(v.clone());
                        i += 2;
                        continue;
                    }
                }
                "-prefix" => {
                    if let Some(RawToken::Word(v)) = tokens.get(i + 1) {
                        prefix_length = v.parse::<u16>().ok();
                        i += 2;
                        continue;
                    }
                }
                "-where" => {
                    if let Some(RawToken::Quoted(v)) = tokens.get(i + 1) {
                        where_clause = Some(v.clone());
                        i += 2;
                        continue;
                    }
                }
                "-order" => {
                    if let Some(RawToken::Word(v)) = tokens.get(i + 1) {
                        order = Some(v.clone());
                        i += 2;
                        continue;
                    }
                }
                "-nulls" => {
                    if let Some(RawToken::Word(v)) = tokens.get(i + 1) {
                        nulls = Some(v.clone());
                        i += 2;
                        continue;
                    }
                }
                _ => {}
            }
        }
        i += 1;
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
        index_type,
        prefix_length,
        where_clause,
        order,
        nulls,
    });
    true
}

/// Parse a model-level composite primary-key directive: `-pk tenant_id,id`.
/// Returns true if the tokens were consumed as such. Mirrors `capture_model_index`
/// but populates `model.primary_key` instead of a secondary index. A model may
/// declare at most one composite key; a second `-pk` line is ignored (last wins
/// is avoided — we keep the first non-empty declaration to stay deterministic).
fn capture_model_pk(model: &mut Model, tokens: &[RawToken]) -> bool {
    let (Some(RawToken::Flag(f)), Some(RawToken::Word(w))) = (tokens.first(), tokens.get(1)) else {
        return false;
    };
    if f != "-pk" {
        return false;
    }
    let fields: Vec<String> = w
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if fields.is_empty() {
        return false;
    }
    // Only set when not already declared, so duplicate `-pk` lines are stable.
    if model.primary_key.is_empty() {
        model.primary_key = fields;
    }
    true
}

/// Parse a top-level `view` declaration into a [`View`].
///
/// Syntax:
/// ```text
/// view Name [field type, field type, ...] [-materialized] = SELECT ...
/// ```
/// The projection (`field type` pairs) is optional. The query body is taken
/// verbatim from the source *after* the first `=` (so it preserves case and
/// any characters that aren't tokenized), and continuation lines are appended
/// by the caller while `parsing_view` is active.
fn parse_view_decl(tokens: &[RawToken], line_no: usize, raw: &str) -> Result<View, OsdlError> {
    // `raw` form: `view Name ... = QUERY`. Everything after the first `=`
    // (the separator) is the query start.
    let query = match raw.split_once('=') {
        Some((_, q)) => q.trim().to_string(),
        None => {
            return Err(OsdlError::Parse(
                ParseError::new(
                    "view declaration must use `=` to separate the projection from the query",
                )
                .with_span(span_at(raw, 0, raw.len(), line_no), raw),
            ));
        }
    };

    let name = match tokens.get(1) {
        Some(RawToken::Name(n)) | Some(RawToken::Word(n)) => n.clone(),
        _ => {
            return Err(OsdlError::Parse(
                ParseError::new("expected a view name after `view`")
                    .with_span(span_at(raw, 0, raw.len(), line_no), raw),
            ));
        }
    };

    // Walk tokens from index 2, collecting `field type` pairs until we hit the
    // `=` separator (a lone `=` tokenizes as `Word("=")`) or a `-materialized` flag.
    // Comma tokens (`Word(",")`) between pairs are skipped.
    let mut fields: Vec<ViewField> = Vec::new();
    let mut materialized = false;
    let mut i = 2;
    while i < tokens.len() {
        match &tokens[i] {
            RawToken::Word(w) if w == "=" => break,
            RawToken::Word(w) if w == "," => {
                i += 1;
            }
            RawToken::Flag(f) if f == "-materialized" => {
                materialized = true;
                i += 1;
            }
            RawToken::Word(fname) | RawToken::Name(fname) if i + 1 < tokens.len() => {
                // Expect a following type word; strip a trailing comma that
                // belongs to the comma-separated projection list.
                if let Some(RawToken::Word(fty) | RawToken::Name(fty)) = tokens.get(i + 1) {
                    fields.push(ViewField {
                        name: fname.trim_end_matches(',').to_string(),
                        ty: fty.trim_end_matches(',').to_string(),
                    });
                    i += 2;
                } else {
                    // Trailing name with no type — treat as query start.
                    break;
                }
            }
            _ => break,
        }
    }

    Ok(View {
        name,
        fields,
        query,
        materialized,
        line: line_no,
    })
}

/// Parse a single seed row from a raw continuation line.
///
/// A row is a whitespace-separated list of `column=value` pairs. The value may
/// be a quoted string (`email="a@b.c"`) — the surrounding quotes are stripped —
/// or a bare token (`active=true`, `age=21`, `id=000...001`). Values are kept
/// verbatim (untyped) for emission into `INSERT`/Mongo-insert statements.
fn parse_seed_row(raw: &str, line_no: usize) -> Result<SeedRow, OsdlError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(SeedRow::default());
    }
    let mut columns: Vec<(String, String)> = Vec::new();
    for part in trimmed.split_whitespace() {
        match part.split_once('=') {
            Some((col, val)) => {
                let col = col.trim().to_string();
                if col.is_empty() {
                    return Err(OsdlError::Parse(ParseError::new(format!(
                        "seed row column is empty before `=` on line {line_no}"
                    ))));
                }
                let val = val.trim();
                // Strip a single layer of surrounding double quotes.
                let val = if val.len() >= 2 && val.starts_with('"') && val.ends_with('"') {
                    val[1..val.len() - 1].to_string()
                } else {
                    val.to_string()
                };
                columns.push((col, val));
            }
            None => {
                return Err(OsdlError::Parse(ParseError::new(format!(
                    "seed row entry `{part}` is missing `=` (expected `column=value`) on line {line_no}"
                ))));
            }
        }
    }
    Ok(SeedRow { columns })
}

/// Shared lookup context for field parsing: the set of known model names and
/// the custom-type table collected during the pre-pass.
struct FieldEnv<'a> {
    known_models: &'a HashSet<String>,
    custom_types: &'a std::collections::HashMap<String, CustomType>,
}

fn add_field_from_tokens(
    model: &mut Model,
    tokens: &[RawToken],
    env: &FieldEnv<'_>,
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
    // Numeric precision/scale, set by `-precision N` / `-scale N` (or
    // `-precision p,s`). `None` means "unset" -> documented per-dialect default.
    let mut numeric_precision: Option<u16> = None;
    let mut numeric_scale: Option<u16> = None;
    let mut capturing_precision = false;
    let mut capturing_scale = false;
    let mut through_model: Option<String> = None;
    let mut capturing_through = false;
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
                if f == "-hasone" {
                    // One-to-one: `posts -hasone Post`. The target model is the
                    // field's type (already captured as a `relation:` ref or a
                    // `Ref`), so `-hasone` only tags the field with `HasOne`
                    // intent for 1:1 cardinality.
                    intents.push(Intent::HasOne);
                    continue;
                }
                if f == "-through" {
                    // Explicit join model for a relation/m2m: `... -through Join`.
                    // The join model name follows as a separate word/reference token.
                    capturing_through = true;
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
                if f == "-precision" {
                    // The value follows as a separate word token: `18` or `18,4`.
                    capturing_precision = true;
                    continue;
                }
                if f == "-scale" {
                    // The value follows as a separate word token: `4`.
                    capturing_scale = true;
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
                if capturing_through {
                    capturing_through = false;
                    through_model = Some(rm.clone());
                    continue;
                }
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
                if capturing_through {
                    capturing_through = false;
                    through_model = Some(w.trim().to_string());
                    continue;
                }
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
                if capturing_precision {
                    capturing_precision = false;
                    // Accept `18` or `18,4` (the latter also sets scale).
                    let (p, s) = w
                        .split_once(',')
                        .map(|(p, s)| (p.trim(), Some(s.trim())))
                        .unwrap_or((w.trim(), None));
                    numeric_precision = Some(p.parse::<u16>().map_err(|_| {
                        OsdlError::Parse(
                            ParseError::new(format!("invalid -precision value `{w}`"))
                                .with_span(span_at(src, byte_start, byte_end, line_no), src),
                        )
                    })?);
                    if let Some(s) = s {
                        numeric_scale = Some(s.parse::<u16>().map_err(|_| {
                            OsdlError::Parse(
                                ParseError::new(format!("invalid -precision scale `{s}`"))
                                    .with_span(span_at(src, byte_start, byte_end, line_no), src),
                            )
                        })?);
                    }
                    continue;
                }
                if capturing_scale {
                    capturing_scale = false;
                    numeric_scale = Some(w.trim().parse::<u16>().map_err(|_| {
                        OsdlError::Parse(
                            ParseError::new(format!("invalid -scale value `{w}`"))
                                .with_span(span_at(src, byte_start, byte_end, line_no), src),
                        )
                    })?);
                    continue;
                }
                if let Some(ct) = env.custom_types.get(w) {
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
                } else if env.known_models.contains(w) {
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
        None => infer_field_type(&name, env.known_models, &model.name),
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
        numeric_precision,
        numeric_scale,
        through_model,
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

/// Apply one `config`-block line (already tokenized) to the accumulator.
/// Keys are matched case-insensitively; unknown keys are ignored so evolving
/// the config schema never rejects an older file.
fn apply_config_line(cfg: &mut SchemaConfig, tokens: &[RawToken]) {
    let mut iter = tokens.iter();
    let key = match iter.next() {
        Some(RawToken::Name(n)) | Some(RawToken::Word(n)) => n.clone(),
        _ => return,
    };
    let mut values: Vec<String> = Vec::new();
    for t in iter {
        match t {
            RawToken::Word(w) | RawToken::Name(w) => values.push(w.clone()),
            RawToken::Flag(f) => values.push(f.clone()),
            RawToken::Quoted(q) => values.push(q.clone()),
            _ => {}
        }
    }
    match key.to_ascii_lowercase().as_str() {
        "default-type" | "default_type" => {
            cfg.default_type = values.first().cloned();
        }
        "timestamp-format" | "timestamp_format" => {
            cfg.timestamp_format = values.first().cloned();
        }
        "soft-delete" | "soft_delete" => {
            // `soft-delete` or `soft-delete field=deleted_at`.
            if let Some(v) = values.first() {
                if let Some(field) = v.strip_prefix("field=") {
                    cfg.soft_delete_field = Some(field.to_string());
                } else {
                    cfg.soft_delete_field = Some(v.clone());
                }
            }
        }
        "audit" => {
            // `audit created_at,updated_at` (comma-joined or space-separated).
            let mut fields = Vec::new();
            for v in &values {
                for part in v.split(',') {
                    let p = part.trim();
                    if !p.is_empty() {
                        fields.push(p.to_string());
                    }
                }
            }
            cfg.audit_fields = fields;
        }
        _ => {}
    }
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
    // Capture the original line text before `pair` is consumed by `into_inner`.
    let raw = pair.as_str().to_string();

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
                        Rule::word => tokens.push(RawToken::Word(b.as_str().to_string())),
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
                        Rule::view_line => {
                            // `view Name (field type, ...) [-materialized] = QUERY`.
                            // Reconstruct the same token stream `parse_view_decl`
                            // expects: `view`, head words/flags, a `=` sentinel,
                            // and the query as a word (the query is actually read
                            // from the raw line text, so its exact tokenization
                            // does not matter).
                            tokens.push(RawToken::Word("view".to_string()));
                            let mut query = String::new();
                            for v in b.into_inner() {
                                match v.as_rule() {
                                    Rule::view_head => {
                                        for h in v.into_inner() {
                                            match h.as_rule() {
                                                Rule::word | Rule::view_word => tokens
                                                    .push(RawToken::Word(h.as_str().to_string())),
                                                Rule::comma => {}
                                                Rule::view_flag => tokens
                                                    .push(RawToken::Flag(h.as_str().to_string())),
                                                _ => {}
                                            }
                                        }
                                    }
                                    Rule::equals => {
                                        tokens.push(RawToken::Word("=".to_string()));
                                    }
                                    Rule::view_query => {
                                        query = v.as_str().to_string();
                                    }
                                    _ => {}
                                }
                            }
                            if !query.is_empty() {
                                tokens.push(RawToken::Word(query));
                            }
                        }
                        Rule::seed_line => {
                            // `seed Model`. Reconstruct the same token stream
                            // `parse_seed_decl` expects: `seed`, then the model
                            // name. The rows live on following continuation lines
                            // and are captured verbatim from `pl.raw` by the
                            // main loop, so they need no tokenization here.
                            tokens.push(RawToken::Word("seed".to_string()));
                            for s in b.into_inner() {
                                if s.as_rule() == Rule::ident {
                                    tokens.push(RawToken::Word(s.as_str().to_string()));
                                }
                            }
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
        raw,
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

    #[test]
    fn parses_seed_rows() {
        let src = "User\n  id uuid -pk\n  email string\n\
seed User\n  id=00000000-0000-0000-0000-000000000001 email=\"root@osdl.dev\"\n  id=00000000-0000-0000-0000-000000000002 email=\"user@osdl.dev\"\n";
        let ast = parse(src).unwrap();
        let seed = ast.seed_by_name("User").expect("seed present");
        assert_eq!(seed.rows.len(), 2);
        let r0 = &seed.rows[0];
        assert_eq!(r0.columns.len(), 2);
        assert_eq!(
            r0.columns,
            vec![
                (
                    "id".to_string(),
                    "00000000-0000-0000-0000-000000000001".to_string()
                ),
                ("email".to_string(), "root@osdl.dev".to_string()),
            ]
        );
        // Column order is preserved within a row; the second row is distinct.
        let r1 = &seed.rows[1];
        assert_eq!(r1.columns[1].1, "user@osdl.dev");
    }

    #[test]
    fn seed_with_multiple_columns_and_bare_values() {
        let src = "Post\n  id uuid -pk\n  title string\n  published bool\n\
seed Post\n  id=abc title=\"Hello\" published=true\n";
        let ast = parse(src).unwrap();
        let seed = ast.seed_by_name("Post").expect("seed present");
        assert_eq!(seed.rows.len(), 1);
        let cols = &seed.rows[0].columns;
        assert_eq!(
            cols,
            &vec![
                ("id".to_string(), "abc".to_string()),
                ("title".to_string(), "Hello".to_string()),
                ("published".to_string(), "true".to_string()),
            ]
        );
    }

    #[test]
    fn seed_requires_equals_in_row() {
        let src = "User\n  id uuid -pk\n\
seed User\n  id=1 brokenrow\n";
        // The malformed row (no `=`) must surface as a parse error.
        assert!(parse(src).is_err());
    }

    #[test]
    fn parse_project_merges_modules() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("osdl_proj_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mod_dir = dir.join("billing");
        let _ = std::fs::create_dir_all(&mod_dir);

        // Entry file: defines User and a view, and imports the billing module.
        let entry = "use billing::invoice\n\
User\n  id uuid -pk\n  email string\n\
view ActiveUsers = SELECT u.id FROM users u WHERE u.active\n\
type Email = string -check \"x ~ 'y'\"\n";
        // Imported module: defines Invoice (model), a view, and a seed.
        let invoice = "Invoice\n  id uuid -pk\n  total int\n\
view RecentInvoices = SELECT i.id FROM invoices i ORDER BY i.created_at DESC\n\
seed Invoice\n  id=00000000-0000-0000-0000-0000000000aa total=42\n";

        let entry_path = dir.join("main.osdl");
        let invoice_path = mod_dir.join("invoice.osdl");
        {
            let mut f = std::fs::File::create(&entry_path).unwrap();
            f.write_all(entry.as_bytes()).unwrap();
            let mut f = std::fs::File::create(&invoice_path).unwrap();
            f.write_all(invoice.as_bytes()).unwrap();
        }

        let project = parse_project(&entry_path).expect("project resolves");
        // Both source files are tracked (entry first, then the import).
        assert_eq!(project.sources.len(), 2);

        let ast = &project.ast;
        // Models merged across files.
        assert!(ast.model_by_name("User").is_some());
        assert!(ast.model_by_name("Invoice").is_some());
        // Views merged across files.
        assert!(ast.view_by_name("ActiveUsers").is_some());
        assert!(ast.view_by_name("RecentInvoices").is_some());
        // Seeds merged across files.
        assert!(ast.seed_by_name("Invoice").is_some());
        // Custom types merged across files.
        assert!(ast.custom_type_by_name("Email").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_project_detects_duplicate_model_across_files() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("osdl_dup_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let mod_dir = dir.join("billing");
        let _ = std::fs::create_dir_all(&mod_dir);

        let entry = "use billing::invoice\nUser\n  id uuid -pk\n";
        let invoice = "User\n  id uuid -pk\n  name string\n";
        let entry_path = dir.join("main.osdl");
        let invoice_path = mod_dir.join("invoice.osdl");
        {
            let mut f = std::fs::File::create(&entry_path).unwrap();
            f.write_all(entry.as_bytes()).unwrap();
            let mut f = std::fs::File::create(&invoice_path).unwrap();
            f.write_all(invoice.as_bytes()).unwrap();
        }
        assert!(parse_project(&entry_path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
