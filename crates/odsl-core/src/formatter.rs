//! Deterministic ODSL formatter.
//!
//! `format_odsl` parses a document, validates it, and re-emits it in a single
//! strictly canonical layout so that two semantically-identical schemas always
//! produce byte-for-byte identical source. This is what powers `odsl fmt`.
//!
//! Canonical rules:
//! * Models are emitted in alphabetical order.
//! * The primary key field is emitted first within a model; all other fields
//!   follow in alphabetical order by name.
//! * Intents on a field are emitted in a fixed, alphabetical order.
//! * Two-space indentation; one blank line between models; no trailing spaces.

use crate::ast::{Ast, Field, Model, ModelIndex, Seed, View};
use crate::types::FieldType;

/// Format an already-parsed, already-validated AST into its canonical source.
///
/// The parse + validate step lives in the caller (typically the CLI, which has
/// access to `odsl-parser`); this function only performs the deterministic
/// re-emission. Re-emitting from an `Ast` guarantees that two
/// semantically-identical schemas always produce byte-for-byte identical source,
/// which is what powers `odsl fmt`.
pub fn format_ast(ast: &Ast) -> String {
    render(ast)
}

fn render(ast: &Ast) -> String {
    let mut models: Vec<&Model> = ast.models().map(|(_, m)| m).collect();
    models.sort_by_key(|m| m.name.clone());

    let mut out = String::new();
    // Schema-level `config` block (if present) is emitted first.
    let cfg = render_config(ast);
    if !cfg.is_empty() {
        out.push_str(&cfg);
        out.push('\n');
    }
    for (i, m) in models.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // Model-level doc comment.
        if let Some(doc) = ast.model_doc(&m.name) {
            for line in doc.lines() {
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(&m.name);
        out.push('\n');
        out.push_str(&render_fields(m, ast));
        out.push_str(&render_indexes(m));
    }
    // Views (read-models) are emitted after the models, each on its own block
    // separated by a blank line, sorted by name for determinism.
    let views = render_views(ast);
    if !views.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&views);
    }
    // Seeds are emitted last, after views, each on its own block separated by a
    // blank line, sorted by target model name for determinism.
    let seeds = render_seeds(ast);
    if !seeds.is_empty() {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&seeds);
    }
    // Each block ends in a single newline; the inter-block separator above
    // produced exactly one blank line between blocks. No extra trailing
    // newline is needed, so just trim a single trailing blank line if present.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// Render all top-level `view` declarations, sorted by name for determinism.
/// The projection (`field type, ...`) is optional; `-materialized` is appended
/// when set; the query body is emitted verbatim (indented on continuation
/// lines) after the `=` separator.
fn render_views(ast: &Ast) -> String {
    let mut views: Vec<&View> = ast.views().collect();
    views.sort_by_key(|v| v.name.clone());
    let mut out = String::new();
    for (i, v) in views.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("view ");
        out.push_str(&v.name);
        if !v.fields.is_empty() {
            out.push(' ');
            let proj: Vec<String> = v
                .fields
                .iter()
                .map(|f| format!("{} {}", f.name, f.ty))
                .collect();
            out.push_str(&proj.join(", "));
        }
        if v.materialized {
            out.push_str(" -materialized");
        }
        out.push_str(" =\n");
        // Indent every line of the query body by two spaces so it round-trips
        // as a continuation block.
        for line in v.query.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Render all top-level `seed` declarations, sorted by target model name for
/// determinism. Each seed block opens with `seed Model` and emits its rows as
/// indented `column=value` continuation lines (one row per line, columns
/// sorted by name within the row for a stable layout).
fn render_seeds(ast: &Ast) -> String {
    let mut seeds: Vec<&Seed> = ast.seeds().collect();
    seeds.sort_by_key(|s| s.model.clone());
    let mut out = String::new();
    for (i, s) in seeds.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str("seed ");
        out.push_str(&s.model);
        out.push('\n');
        for row in &s.rows {
            let cols = row.columns.clone();
            let entries: Vec<String> = cols
                .iter()
                .map(|(c, v)| {
                    // Quote values that contain whitespace or would otherwise
                    // be ambiguous; bare scalar tokens stay unquoted.
                    if v.contains(char::is_whitespace) {
                        format!("{c}=\"{v}\"")
                    } else {
                        format!("{c}={v}")
                    }
                })
                .collect();
            out.push_str("  ");
            out.push_str(&entries.join(" "));
            out.push('\n');
        }
    }
    out
}

/// Render the schema-level `config` block, or an empty string when no config
/// is present. The block order is fixed for determinism.
fn render_config(ast: &Ast) -> String {
    let c = &ast.config;
    if c.default_type.is_none()
        && c.timestamp_format.is_none()
        && c.soft_delete_field.is_none()
        && c.audit_fields.is_empty()
    {
        return String::new();
    }
    let mut out = String::from("config\n");
    if let Some(dt) = &c.default_type {
        out.push_str(&format!("  default-type {dt}\n"));
    }
    if let Some(tf) = &c.timestamp_format {
        out.push_str(&format!("  timestamp-format {tf}\n"));
    }
    if let Some(sd) = &c.soft_delete_field {
        out.push_str(&format!("  soft-delete field={sd}\n"));
    }
    if !c.audit_fields.is_empty() {
        out.push_str(&format!("  audit {}\n", c.audit_fields.join(",")));
    }
    out
}

fn render_fields(m: &Model, ast: &Ast) -> String {
    let mut fields: Vec<&Field> = m.fields().map(|(_, f)| f).collect();
    // PK first, then alphabetical by name.
    fields.sort_by(|a, b| {
        let a_pk = a.has(crate::types::Intent::Pk);
        let b_pk = b.has(crate::types::Intent::Pk);
        match (a_pk, b_pk) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });

    let mut out = String::new();
    for f in fields {
        // Field-level doc comment.
        if let Some(doc) = ast.field_doc(&m.name, &f.name) {
            for line in doc.lines() {
                out.push_str("  /// ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str("  ");
        out.push_str(&f.name);
        out.push(' ');
        out.push_str(&render_type(f));
        let intents = render_intents(f);
        if !intents.is_empty() {
            out.push(' ');
            out.push_str(&intents.join(" "));
        }
        if !f.enum_variants.is_empty() {
            out.push_str(" -enum ");
            out.push_str(&f.enum_variants.join(","));
        }
        if let Some(d) = &f.default_value {
            out.push_str(" -default ");
            out.push_str(d);
        }
        if let Some(t) = &f.m2m_target {
            out.push_str(" -m2m ");
            out.push_str(t);
        }
        if let Some(join) = &f.through_model {
            out.push_str(" -through ");
            out.push_str(join);
        }
        if let Some(expr) = &f.check_expr {
            out.push_str(" -check \"");
            out.push_str(expr);
            out.push('"');
        }
        if !f.polymorphic_targets.is_empty() {
            out.push_str(" -polymorphic ");
            out.push_str(&f.polymorphic_targets.join(","));
        }
        if let Some(reason) = ast.field_deprecation(&m.name, &f.name) {
            out.push_str(" -deprecated \"");
            out.push_str(reason);
            out.push('"');
        }
        if let Some(p) = f.numeric_precision {
            out.push_str(&format!(" -precision {p}"));
            if let Some(s) = f.numeric_scale {
                out.push_str(&format!(",{s}"));
            }
        } else if let Some(s) = f.numeric_scale {
            out.push_str(&format!(" -scale {s}"));
        }
        out.push('\n');
    }
    out
}

fn render_type(f: &Field) -> String {
    match &f.ty {
        FieldType::Scalar(s) => s.as_keyword().to_string(),
        FieldType::Ref(r) => format!("{}.{}", r.model, r.field),
        // A `-hasone`/`-relation` field stores the target as `relation:Model`.
        // When the field carries `HasOne`, render the bare model name as the
        // type (so the output is `field Model -hasone`, not `relation:Model`).
        FieldType::InferredRef(s) if s.starts_with("relation:") => {
            if f.has(crate::types::Intent::HasOne) {
                s["relation:".len()..].to_string()
            } else {
                format!("relation:{}", &s["relation:".len()..])
            }
        }
        FieldType::InferredRef(s) => s.clone(),
    }
}

fn render_intents(f: &Field) -> Vec<String> {
    use crate::types::Intent;
    // Fixed alphabetical order for stable output. Intents that carry a payload
    // (`-check`, `-polymorphic`) are emitted separately with their value, so
    // they are intentionally excluded here to avoid double emission.
    let order: &[Intent] = &[
        Intent::Pk,
        Intent::Auto,
        Intent::Uniq,
        Intent::Null,
        Intent::Tz,
        Intent::Index,
        Intent::Relation,
        Intent::HasOne,
        Intent::Virtual,
        Intent::SoftDelete,
        Intent::Partition,
        Intent::Default,
        Intent::M2m,
        Intent::Enum,
    ];
    let mut out = Vec::new();
    for intent in order {
        if f.has(*intent) {
            out.push(intent.as_keyword().to_string());
        }
    }
    out
}

fn render_indexes(m: &Model) -> String {
    let mut indexes: Vec<&ModelIndex> = m.indexes.iter().collect();
    indexes.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    for idx in indexes {
        out.push_str("  ");
        out.push_str(if idx.unique { "-uniq " } else { "-index " });
        out.push_str(&idx.fields.join(","));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ast, Field, Model, Seed, SeedRow};
    use crate::types::{FieldType, Intent, ScalarType};

    /// Build an `Ast` from a map of (model -> (field, type_keyword, intents)).
    type FieldSpec<'a> = (&'a str, ScalarType, &'a [Intent]);
    type ModelSpec<'a> = (&'a str, &'a [FieldSpec<'a>]);
    fn build(spec: &[ModelSpec<'_>]) -> Ast {
        let mut ast = Ast::new();
        for (mname, fields) in spec {
            let mut m = Model {
                name: (*mname).to_string(),
                fields: la_arena::Arena::new(),
                field_index: vec![],
                line: 1,
                indexes: vec![],
                primary_key: vec![],
            };
            for (fname, ty, intents) in *fields {
                m.add_field(Field {
                    custom_type: None,
                    name: (*fname).to_string(),
                    ty: FieldType::Scalar(*ty),
                    intents: intents.to_vec(),
                    enum_variants: vec![],
                    default_value: None,
                    m2m_target: None,
                    check_expr: None,
                    polymorphic_targets: vec![],
                    on_delete: None,
                    on_update: None,
                    numeric_precision: None,
                    numeric_scale: None,
                    through_model: None,
                    line: 1,
                });
            }
            ast.add_model(m);
        }
        ast
    }

    #[test]
    fn formats_and_normalizes_order() {
        let ast = build(&[
            (
                "Post",
                &[
                    ("title", ScalarType::String, &[]),
                    ("id", ScalarType::Uuid, &[Intent::Pk]),
                ],
            ),
            (
                "User",
                &[
                    ("email", ScalarType::String, &[Intent::Uniq]),
                    ("id", ScalarType::Uuid, &[Intent::Pk]),
                ],
            ),
        ]);
        let out = format_ast(&ast);
        let expected =
            "Post\n  id uuid -pk\n  title string\n\nUser\n  id uuid -pk\n  email string -uniq\n";
        assert_eq!(out, expected, "got:\n{out}");
    }

    #[test]
    fn preserves_polymorphic_and_check() {
        let mut ast = build(&[
            (
                "Comment",
                &[
                    ("id", ScalarType::Uuid, &[Intent::Pk]),
                    ("age", ScalarType::Int, &[]),
                ],
            ),
            ("Video", &[("id", ScalarType::Uuid, &[Intent::Pk])]),
            ("Post", &[("id", ScalarType::Uuid, &[Intent::Pk])]),
        ]);
        // Inject the polymorphic + check fields manually (not expressible via build()).
        {
            let c = ast.model_by_name("Comment").unwrap();
            let m = &mut ast.models[c];
            m.add_field(Field {
                custom_type: None,
                name: "target".into(),
                ty: FieldType::Scalar(ScalarType::String),
                intents: vec![Intent::Polymorphic],
                enum_variants: vec![],
                default_value: None,
                m2m_target: None,
                check_expr: None,
                polymorphic_targets: vec!["Post".into(), "Video".into()],
                on_delete: None,
                on_update: None,
                numeric_precision: None,
                numeric_scale: None,
                through_model: None,
                line: 1,
            });
            let f = m.field_by_name("age").unwrap();
            let f = &mut m.fields[f];
            f.check_expr = Some("age >= 0".into());
        }
        let out = format_ast(&ast);
        assert!(out.contains("target string -polymorphic Post,Video"));
        assert!(out.contains("-check \"age >= 0\""));
    }

    #[test]
    fn idempotent_on_rebuilt_ast() {
        // Building the same spec twice yields the same canonical text.
        let a = format_ast(&build(&[(
            "User",
            &[
                ("email", ScalarType::String, &[Intent::Uniq]),
                ("id", ScalarType::Uuid, &[Intent::Pk]),
            ],
        )]));
        let b = format_ast(&build(&[(
            "User",
            &[
                ("id", ScalarType::Uuid, &[Intent::Pk]),
                ("email", ScalarType::String, &[Intent::Uniq]),
            ],
        )]));
        assert_eq!(a, b);
    }

    #[test]
    fn preserves_doc_comments_and_deprecation() {
        // Build an AST with docs/deprecation in the side-maps and format it.
        let mut ast = build(&[
            (
                "User",
                &[
                    ("id", ScalarType::Uuid, &[Intent::Pk]),
                    ("email", ScalarType::String, &[Intent::Uniq]),
                ],
            ),
            ("Post", &[("id", ScalarType::Uuid, &[Intent::Pk])]),
        ]);
        ast.model_docs
            .insert("User".into(), "A registered account holder.".into());
        ast.field_docs.insert(
            ("User".into(), "email".into()),
            "The user's primary email address.".into(),
        );
        ast.field_deprecated
            .insert(("User".into(), "email".into()), "use this".into());
        let out = format_ast(&ast);
        assert!(out.contains("/// A registered account holder."));
        assert!(out.contains("/// The user's primary email address."));
        assert!(out.contains("email string -uniq -deprecated \"use this\""));
        // The deprecation reason is preserved on the field.
        assert_eq!(ast.field_deprecation("User", "email"), Some("use this"));
    }

    #[test]
    fn formats_config_block() {
        // Build an Ast with a config block and verify the formatter emits it
        // first, in the fixed canonical order. (Parse -> format -> parse
        // round-trip stability is covered in the parser crate's tests.)
        let mut ast = Ast::new();
        ast.config.default_type = Some("uuid".into());
        ast.config.timestamp_format = Some("iso8601".into());
        ast.config.soft_delete_field = Some("deleted_at".into());
        ast.config.audit_fields = vec!["created_at".into(), "updated_at".into()];
        ast.add_model(Model {
            name: "User".into(),
            fields: la_arena::Arena::new(),
            field_index: vec![],
            line: 1,
            indexes: vec![],
            primary_key: vec![],
        });
        let out = format_ast(&ast);
        assert!(out.starts_with("config\n"));
        assert!(out.contains("  default-type uuid\n"));
        assert!(out.contains("  timestamp-format iso8601\n"));
        assert!(out.contains("  soft-delete field=deleted_at\n"));
        assert!(out.contains("  audit created_at,updated_at\n"));
        // The config block precedes the model block.
        assert!(out.find("config").unwrap() < out.find("User").unwrap());
    }

    #[test]
    fn formats_seed_block() {
        let mut ast = build(&[(
            "User",
            &[
                ("id", ScalarType::Uuid, &[Intent::Pk]),
                ("email", ScalarType::String, &[]),
            ],
        )]);
        ast.add_seed(Seed {
            model: "User".into(),
            rows: vec![
                SeedRow {
                    columns: vec![
                        ("id".into(), "00000000-0000-0000-0000-000000000001".into()),
                        ("email".into(), "root@odsl.dev".into()),
                    ],
                },
                SeedRow {
                    columns: vec![
                        ("id".into(), "00000000-0000-0000-0000-000000000002".into()),
                        ("email".into(), "user@odsl.dev".into()),
                    ],
                },
            ],
            line: 5,
        });
        let out = format_ast(&ast);
        // The seed block follows the model block.
        assert!(out.contains("seed User\n"));
        assert!(out.contains("  id=00000000-0000-0000-0000-000000000001 email=root@odsl.dev"));
        assert!(out.contains("  id=00000000-0000-0000-0000-000000000002 email=user@odsl.dev"));
        assert!(out.find("User\n").unwrap() < out.find("seed User").unwrap());
    }
}
