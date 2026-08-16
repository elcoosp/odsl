//! Deterministic OSDL formatter.
//!
//! `format_osdl` parses a document, validates it, and re-emits it in a single
//! strictly canonical layout so that two semantically-identical schemas always
//! produce byte-for-byte identical source. This is what powers `osdl fmt`.
//!
//! Canonical rules:
//! * Models are emitted in alphabetical order.
//! * The primary key field is emitted first within a model; all other fields
//!   follow in alphabetical order by name.
//! * Intents on a field are emitted in a fixed, alphabetical order.
//! * Two-space indentation; one blank line between models; no trailing spaces.

use crate::ast::{Ast, Field, Model, ModelIndex};
use crate::types::FieldType;

/// Format an already-parsed, already-validated AST into its canonical source.
///
/// The parse + validate step lives in the caller (typically the CLI, which has
/// access to `osdl-parser`); this function only performs the deterministic
/// re-emission. Re-emitting from an `Ast` guarantees that two
/// semantically-identical schemas always produce byte-for-byte identical source,
/// which is what powers `osdl fmt`.
pub fn format_ast(ast: &Ast) -> String {
    render(ast)
}

fn render(ast: &Ast) -> String {
    let mut models: Vec<&Model> = ast.models().map(|(_, m)| m).collect();
    models.sort_by_key(|m| m.name.clone());

    let mut out = String::new();
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
    // Each model block ends in a single newline; the inter-model separator
    // above produced exactly one blank line between blocks. No extra trailing
    // newline is needed, so just trim a single trailing blank line if present.
    while out.ends_with("\n\n") {
        out.pop();
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
        FieldType::InferredRef(s) if s.starts_with("relation:") => {
            format!("relation:{}", &s["relation:".len()..])
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
    use crate::ast::{Ast, Field, Model};
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
}
