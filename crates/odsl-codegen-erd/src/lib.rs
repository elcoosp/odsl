//! ERD (entity-relationship diagram) renderers: Mermaid `erDiagram` and DBML.
//!
//! Both formats are generated from the same arena [`Ast`]: every model becomes
//! a table/node, scalar fields become columns/attributes, and foreign-key
//! references (`Model.field`) become relationship edges. Many-to-many
//! relationships surface naturally as `<Source>_<Target>` junction tables with
//! two outgoing references, so no special-casing is required.

use odsl_core::ast::{Ast, LockField, LockModel, LockView};

/// Supported ERD output dialects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErdFormat {
    /// Mermaid `erDiagram` (Markdown-embeddable, git-friendly).
    Mermaid,
    /// DBML (dbdiagram.io compatible).
    Dbml,
}

impl ErdFormat {
    /// Parse a CLI `--format` value (case-insensitive, tolerant of `-`/`_`).
    pub fn from_cli(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
            "mermaid" => Some(ErdFormat::Mermaid),
            "dbml" | "dbdiagram" => Some(ErdFormat::Dbml),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ErdFormat::Mermaid => "mermaid",
            ErdFormat::Dbml => "dbml",
        }
    }
}

/// Render the schema to the requested ERD dialect.
///
/// Rendering is driven by the *expanded* lockfile projection (`ast.to_lock()`),
/// which materialises many-to-many relationships into `<Source>_<Target>`
/// junction tables and drops the original `-m2m` marker field. This keeps the
/// diagram faithful to what is actually deployed.
pub fn render(ast: &Ast, format: ErdFormat) -> Result<(String, String), String> {
    let models = ast.to_lock();
    let views = ast.to_lock_views();
    match format {
        ErdFormat::Mermaid => {
            let body = render_mermaid(&models, &views);
            Ok(("schema.erd.md".to_string(), body))
        }
        ErdFormat::Dbml => {
            let body = render_dbml(&models, &views);
            Ok(("schema.dbml".to_string(), body))
        }
    }
}

/// Map a lockfield's stored type string to its ERD display type. References
/// (`Model.field`) collapse to the referenced model name; scalars pass through.
fn field_type_label(ty: &str) -> String {
    match ty.split_once('.') {
        Some((model, _)) => model.to_string(),
        None => ty.to_string(),
    }
}

/// Whether a lockfield is a foreign-key reference (`Model.field`).
fn is_ref(ty: &str) -> bool {
    ty.contains('.')
}

fn has_intent(intents: &[String], kw: &str) -> bool {
    intents.iter().any(|i| i == kw)
}

// ---------------------------------------------------------------------------
// Mermaid
// ---------------------------------------------------------------------------

fn render_mermaid(models: &[LockModel], views: &[LockView]) -> String {
    let mut out = String::new();
    out.push_str("erDiagram\n");

    let mut names: Vec<&String> = models.iter().map(|m| &m.name).collect();
    names.sort();
    names.dedup();

    for name in &names {
        let model = models
            .iter()
            .find(|m| &m.name == *name)
            .expect("model exists");
        out.push_str(&format!("    \"{}\" {{\n", model.name));
        let mut fields: Vec<&LockField> = model.fields.iter().collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        for f in &fields {
            // Skip many-to-many marker fields (the junction table carries the
            // relationship instead).
            if f.m2m_target.is_some() {
                continue;
            }
            let mut tags: Vec<&str> = Vec::new();
            if model.primary_key.contains(&f.name) {
                tags.push("PK");
            }
            if is_ref(&f.ty) {
                tags.push("FK");
            }
            if has_intent(&f.intents, "-uniq") {
                tags.push("UK");
            }
            let tag = if tags.is_empty() {
                String::new()
            } else {
                format!(" {}", tags.join(", "))
            };
            out.push_str(&format!(
                "        {} {}{}\n",
                field_type_label(&f.ty),
                f.name,
                tag
            ));
        }
        out.push_str("    }\n");
    }

    // Relationship edges (parent ||--o{ child). Junction tables surface as
    // two outgoing references, which is exactly the many-to-many shape.
    for model in models {
        let mut fields: Vec<&LockField> = model.fields.iter().collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        for f in &fields {
            if let Some((parent, _)) = f.ty.split_once('.') {
                out.push_str(&format!(
                    "    {} ||--o{{ {} : \"{}\"\n",
                    parent, model.name, f.name
                ));
            }
        }
    }

    // Views (read-models) are rendered as Mermaid class nodes marked `<<View>>`
    // so they are visually distinct from base tables in the ERD.
    for v in views {
        out.push_str(&format!("    \"{}\" {{\n", v.name));
        out.push_str("        <<View>>\n");
        if !v.fields.is_empty() {
            for (fname, fty) in &v.fields {
                out.push_str(&format!("        {} {}\n", fty, fname));
            }
        } else {
            out.push_str(&format!(
                "        // derived from: {}\n",
                v.query.lines().next().unwrap_or("").trim()
            ));
        }
        out.push_str("    }\n");
    }

    out
}

// ---------------------------------------------------------------------------
// DBML
// ---------------------------------------------------------------------------

fn render_dbml(models: &[LockModel], views: &[LockView]) -> String {
    let mut out = String::new();
    out.push_str("// Generated by `odsl erd --format dbml`. Do not edit by hand.\n");
    out.push_str("// Source of truth: the ODSL schema.\n\n");

    let mut names: Vec<&String> = models.iter().map(|m| &m.name).collect();
    names.sort();
    names.dedup();

    for name in &names {
        let model = models
            .iter()
            .find(|m| &m.name == *name)
            .expect("model exists");
        out.push_str(&format!("Table {} {{\n", model.name));
        let mut fields: Vec<&LockField> = model.fields.iter().collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        for f in &fields {
            if f.m2m_target.is_some() {
                continue;
            }
            let required = if has_intent(&f.intents, "-null") {
                ""
            } else {
                " [not null]"
            };
            let pk = if model.primary_key.contains(&f.name) {
                " [primary key]"
            } else {
                ""
            };
            let uniq = if has_intent(&f.intents, "-uniq") {
                " [unique]"
            } else {
                ""
            };
            out.push_str(&format!(
                "  {} {}{}{}{}\n",
                f.name,
                field_type_label(&f.ty),
                required,
                pk,
                uniq
            ));
        }
        // Model-level composite indexes.
        for idx in &model.indexes {
            let cols = idx
                .fields
                .iter()
                .map(|c| format!("({c})"))
                .collect::<Vec<_>>()
                .join(", ");
            if idx.unique {
                out.push_str(&format!("  indexes {{\n    {} [unique]\n  }}\n", cols));
            } else {
                out.push_str(&format!("  indexes {{\n    {}\n  }}\n", cols));
            }
        }
        // Primary key (composite when model.primary_key has >1 column).
        if !model.primary_key.is_empty() {
            let cols = model.primary_key.join(", ");
            out.push_str(&format!("  primary key ({cols})\n"));
        }
        out.push_str("}\n\n");
    }

    // References. Junction tables render as two outgoing refs → many-to-many.
    for model in models {
        let mut fields: Vec<&LockField> = model.fields.iter().collect();
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        for f in &fields {
            if let Some((parent, col)) = f.ty.split_once('.') {
                out.push_str(&format!(
                    "Ref: {}.{} > {}.{}\n",
                    model.name, f.name, parent, col
                ));
            }
        }
    }

    // Views (read-models) are emitted as DBML tables tagged `// view` so they
    // are distinguishable from base tables. The projection (when present) is
    // listed as columns; otherwise the query is captured as a comment.
    for v in views {
        out.push_str(&format!("Table {} {{ // view\n", v.name));
        if !v.fields.is_empty() {
            for (fname, fty) in &v.fields {
                out.push_str(&format!("  {} {}\n", fname, fty));
            }
        } else {
            out.push_str(&format!(
                "  // derived from: {}\n",
                v.query.lines().next().unwrap_or("").trim()
            ));
        }
        if v.materialized {
            out.push_str("  // materialized\n");
        }
        out.push_str("}\n\n");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use odsl_parser::parse;

    const SCHEMA: &str = r#"
User
  id uuid -pk
  email string -uniq

Post
  id uuid -pk
  author User.id
  title string
"#;

    #[test]
    fn mermaid_contains_models_and_relationship() {
        let ast = parse(SCHEMA).expect("parse");
        let (name, body) = render(&ast, ErdFormat::Mermaid).unwrap();
        assert_eq!(name, "schema.erd.md");
        assert!(body.contains("erDiagram"));
        assert!(body.contains("\"User\""));
        assert!(body.contains("\"Post\""));
        assert!(body.contains("id PK"));
        assert!(body.contains("author FK"));
        // parent ||--o{ child : "author"
        assert!(body.contains("User ||--o{ Post : \"author\""));
    }

    #[test]
    fn dbml_contains_tables_and_ref() {
        let ast = parse(SCHEMA).expect("parse");
        let (name, body) = render(&ast, ErdFormat::Dbml).unwrap();
        assert_eq!(name, "schema.dbml");
        assert!(body.contains("Table User {"));
        assert!(body.contains("Table Post {"));
        assert!(body.contains("id uuid [not null] [primary key]"));
        assert!(body.contains("Ref: Post.author > User.id"));
    }

    #[test]
    fn m2m_expands_to_junction_table() {
        let ast = parse("User\n  id uuid -pk\n  groups -m2m Group\n\nGroup\n  id uuid -pk\n")
            .expect("parse");
        let (_, mermaid) = render(&ast, ErdFormat::Mermaid).unwrap();
        // The junction table should be present and the marker field absent.
        assert!(mermaid.contains("\"User_Group\""));
        assert!(!mermaid.contains("groups FK"));
        // Two outgoing refs from the junction -> many-to-many.
        assert!(mermaid.contains("User ||--o{ User_Group :"));
        assert!(mermaid.contains("Group ||--o{ User_Group :"));
    }

    #[test]
    fn composite_pk_marks_both_columns_and_dbml() {
        let src = "Membership
  tenant_id uuid
  user_id uuid
  role string
  -pk tenant_id,user_id
";
        let ast = parse(src).expect("parse");
        // Mermaid: both key columns tagged PK.
        let (_, mermaid) = render(&ast, ErdFormat::Mermaid).unwrap();
        assert!(mermaid.contains("tenant_id PK"));
        assert!(mermaid.contains("user_id PK"));
        // DBML: a composite primary key declaration.
        let (_, dbml) = render(&ast, ErdFormat::Dbml).unwrap();
        assert!(
            dbml.contains("primary key (tenant_id, user_id)"),
            "got:\n{dbml}"
        );
    }

    #[test]
    fn round_trip_format_parse() {
        assert_eq!(ErdFormat::from_cli("mermaid"), Some(ErdFormat::Mermaid));
        assert_eq!(ErdFormat::from_cli("DBML"), Some(ErdFormat::Dbml));
        assert_eq!(ErdFormat::from_cli("db-diagram"), Some(ErdFormat::Dbml));
        assert_eq!(ErdFormat::from_cli("svg"), None);
    }
}
