//! TypeScript runtime validator renderers: Zod, Valibot, and TypeBox.
//!
//! Each OSDL model becomes a schema object in the requested library, with
//! scalar types, nullability, enums, references, unique/check/default
//! annotations, polymorphic expansions, and `///` doc / `-deprecated` carried
//! over. This is the natural follow-on to the plain TypeScript renderer: the
//! same single source of truth now yields *enforceable* runtime schemas.
//!
//! The three flavours are thin layers over the identical field metadata, so
//! they are emitted by one crate with a shared field-mapping core.

#![allow(clippy::result_large_err)]

use osdl_core::Target;
use osdl_core::ast::{Ast, Field, Model};
use osdl_core::errors::OsdlError;
use osdl_core::types::{FieldType, Intent, ScalarType};
use osdl_core::validator::CodeRenderer;

/// Which validator library to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidatorFlavor {
    Zod,
    Valibot,
    TypeBox,
}

impl ValidatorFlavor {
    pub fn as_str(self) -> &'static str {
        match self {
            ValidatorFlavor::Zod => "zod",
            ValidatorFlavor::Valibot => "valibot",
            ValidatorFlavor::TypeBox => "typebox",
        }
    }
}

/// A renderer for one of the three TS validator flavours.
pub struct TsValidatorRenderer {
    target: Target,
    flavor: ValidatorFlavor,
}

impl TsValidatorRenderer {
    pub fn new(target: Target, flavor: ValidatorFlavor) -> Self {
        Self { target, flavor }
    }
}

impl CodeRenderer for TsValidatorRenderer {
    fn target(&self) -> Target {
        self.target
    }

    fn render(&self, ast: &Ast) -> Result<Vec<(String, String)>, OsdlError> {
        let mut files = Vec::new();
        let mut names: Vec<String> = ast.models().map(|(_, m)| m.name.clone()).collect();
        names.sort();
        for name in &names {
            let (_, model) = ast
                .models()
                .find(|(_, m)| &m.name == name)
                .expect("model exists");
            files.push((
                format!("{}.{}", name, self.flavor.as_str()),
                render_model(self.flavor, model, ast),
            ));
        }
        let barrel = render_barrel(self.flavor, &names);
        files.push((format!("index.{}", self.flavor.as_str()), barrel));
        Ok(files)
    }
}

/// Map an OSDL scalar to the flavour-specific primitive expression (without
/// nullability/optional wrappers, which are applied by the caller).
fn scalar_expr(flavor: ValidatorFlavor, s: ScalarType) -> String {
    match flavor {
        ValidatorFlavor::Zod => match s {
            ScalarType::String => "z.string()".into(),
            ScalarType::Int => "z.number().int()".into(),
            ScalarType::BigInt => "z.number()".into(),
            ScalarType::Float => "z.number()".into(),
            ScalarType::Bool => "z.boolean()".into(),
            ScalarType::DateTime => "z.string().datetime()".into(),
            ScalarType::Date => "z.string()".into(),
            ScalarType::Uuid => "z.string().uuid()".into(),
            ScalarType::Json => "z.unknown()".into(),
            ScalarType::Binary => "z.instanceof(Uint8Array)".into(),
            ScalarType::Decimal => "z.number()".into(),
        },
        ValidatorFlavor::Valibot => match s {
            ScalarType::String => "v.string()".into(),
            ScalarType::Int => "v.number()".into(),
            ScalarType::BigInt => "v.number()".into(),
            ScalarType::Float => "v.number()".into(),
            ScalarType::Bool => "v.boolean()".into(),
            ScalarType::DateTime => "v.string()".into(),
            ScalarType::Date => "v.string()".into(),
            ScalarType::Uuid => "v.string()".into(),
            ScalarType::Json => "v.any()".into(),
            ScalarType::Binary => "v.instance(Uint8Array)".into(),
            ScalarType::Decimal => "v.number()".into(),
        },
        ValidatorFlavor::TypeBox => match s {
            ScalarType::String => "Type.String()".into(),
            ScalarType::Int => "Type.Integer()".into(),
            ScalarType::BigInt => "Type.Number()".into(),
            ScalarType::Float => "Type.Number()".into(),
            ScalarType::Bool => "Type.Boolean()".into(),
            ScalarType::DateTime => "Type.String({ format: \"date-time\" })".into(),
            ScalarType::Date => "Type.String({ format: \"date\" })".into(),
            ScalarType::Uuid => "Type.String({ format: \"uuid\" })".into(),
            ScalarType::Json => "Type.Unknown()".into(),
            ScalarType::Binary => "Type.Uint8Array()".into(),
            ScalarType::Decimal => "Type.Number()".into(),
        },
    }
}

/// Wrap a base expression with the flavour-specific nullable/optional marker.
fn with_nullable(flavor: ValidatorFlavor, base: &str, nullable: bool) -> String {
    if !nullable {
        return base.to_string();
    }
    match flavor {
        ValidatorFlavor::Zod => format!("{base}.nullable()"),
        ValidatorFlavor::Valibot => format!("v.nullable({base})"),
        ValidatorFlavor::TypeBox => format!("Type.Optional({base})"),
    }
}

/// Annotate a base expression with the flavour-specific unique marker, if any.
fn with_unique(flavor: ValidatorFlavor, base: &str, unique: bool) -> String {
    if !unique {
        return base.to_string();
    }
    match flavor {
        // A no-op refine would be misleading; annotate with a comment instead.
        ValidatorFlavor::Zod => format!("{base} /* unique */"),
        ValidatorFlavor::Valibot => format!("{base} /* unique */"),
        ValidatorFlavor::TypeBox => format!("{base} /* unique */"),
    }
}

/// Public Zod field expression (reused by the tRPC router renderer, which
/// builds input/output schemas from the same Zod vocabulary).
pub fn zod_field_schema(field: &Field) -> String {
    field_schema(ValidatorFlavor::Zod, field)
}

/// Build the schema expression for a single field (with wrappers applied).
fn field_schema(flavor: ValidatorFlavor, field: &Field) -> String {
    // Polymorphic references expand into a type/id pair.
    if !field.polymorphic_targets.is_empty() {
        let t = format!("{}_type", to_snake(&field.name));
        let i = format!("{}_id", to_snake(&field.name));
        return match flavor {
            ValidatorFlavor::Zod => format!("{{ {t}: z.string(), {i}: z.string() }}", t = t, i = i),
            ValidatorFlavor::Valibot => {
                format!("{{ {t}: v.string(), {i}: v.string() }}", t = t, i = i)
            }
            ValidatorFlavor::TypeBox => {
                format!("{{ {t}: Type.String(), {i}: Type.String() }}", t = t, i = i)
            }
        };
    }

    let base = match &field.ty {
        FieldType::Scalar(s) => scalar_expr(flavor, *s),
        FieldType::Ref(r) => match flavor {
            ValidatorFlavor::Zod => format!("z.string() /* FK -> {} */", r.model),
            ValidatorFlavor::Valibot => format!("v.string() /* FK -> {} */", r.model),
            ValidatorFlavor::TypeBox => format!("Type.String() /* FK -> {} */", r.model),
        },
        FieldType::InferredRef(s) => match flavor {
            ValidatorFlavor::Zod => format!("z.string() /* FK -> {s} */"),
            ValidatorFlavor::Valibot => format!("v.string() /* FK -> {s} */"),
            ValidatorFlavor::TypeBox => format!("Type.String() /* FK -> {s} */"),
        },
    };

    // Enum variants (only for scalar enums).
    let base = if field.has(Intent::Enum) && !field.enum_variants.is_empty() {
        let mut variants: Vec<String> = field.enum_variants.clone();
        variants.retain(|v| !v.is_empty());
        variants.sort();
        let list = variants
            .iter()
            .map(|v| format!("\"{v}\""))
            .collect::<Vec<_>>()
            .join(", ");
        match flavor {
            ValidatorFlavor::Zod => format!("z.enum([{list}])"),
            ValidatorFlavor::Valibot => format!("v.enumType([{list}])"),
            ValidatorFlavor::TypeBox => {
                let literals = variants
                    .iter()
                    .map(|v| format!("Type.Literal(\"{v}\")"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Type.Union([{literals}])")
            }
        }
    } else {
        base
    };

    let nullable = field.has(Intent::Null);
    let unique = field.has(Intent::Uniq);
    let mut expr = with_unique(flavor, &base, unique);
    // Default: Zod/TypeBox/Valibot all support a `.default(...)`-style call;
    // Valibot uses `v.withDefault(x, value)`. Keep it flavour-correct.
    if let Some(value) = &field.default_value {
        let parsed = literal_value(value);
        expr = match flavor {
            ValidatorFlavor::Zod => format!("{expr}.default({parsed})"),
            ValidatorFlavor::Valibot => format!("v.withDefault({expr}, {parsed})"),
            ValidatorFlavor::TypeBox => format!("Type.Optional({expr}, {{ default: {parsed} }})"),
        };
    }
    with_nullable(flavor, &expr, nullable)
}

/// Render a default value literal (numbers/booleans stay native; strings
/// quoted). `now` is a server-side hint, rendered as a string literal.
fn literal_value(raw: &str) -> String {
    match raw {
        "true" => "true".into(),
        "false" => "false".into(),
        "null" => "null".into(),
        "now" => "\"now\"".into(),
        s if s.parse::<i64>().is_ok() || s.parse::<f64>().is_ok() => s.to_string(),
        s => format!("\"{s}\""),
    }
}

/// Build the JSDoc / metadata comment block for a field.
fn field_doc_lines(flavor: ValidatorFlavor, field: &Field, ast: &Ast, model: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut tags: Vec<String> = Vec::new();
    if field.has(Intent::Pk) {
        tags.push("primary key".into());
    }
    if field.has(Intent::Uniq) {
        tags.push("unique".into());
    }
    if field.has(Intent::Virtual) {
        tags.push("virtual (computed/serialized-only)".into());
    }
    if field.has(Intent::SoftDelete) {
        tags.push("soft-delete marker".into());
    }
    if let Some(expr) = &field.check_expr {
        tags.push(format!("check: {expr}"));
    }
    if let Some(def) = &field.default_value {
        tags.push(format!("default: {def}"));
    }
    let doc = ast.field_doc(model, &field.name);
    let deprecated = ast.field_deprecation(model, &field.name);

    if matches!(
        flavor,
        ValidatorFlavor::Zod | ValidatorFlavor::Valibot | ValidatorFlavor::TypeBox
    ) && (!tags.is_empty() || doc.is_some() || deprecated.is_some())
    {
        lines.push("  /**".into());
        for line in doc.iter().flat_map(|d| d.lines()) {
            lines.push(format!("   * {line}"));
        }
        if !tags.is_empty() {
            lines.push(format!("   * {}", tags.join("; ")));
        }
        if let Some(reason) = deprecated {
            lines.push(format!("   * @deprecated {reason}"));
        }
        lines.push("   */".into());
    }
    lines
}

fn render_model(flavor: ValidatorFlavor, model: &Model, ast: &Ast) -> String {
    let import: String = match flavor {
        ValidatorFlavor::Zod => "import { z } from \"zod\";".into(),
        ValidatorFlavor::Valibot => "import * as v from \"valibot\";".into(),
        ValidatorFlavor::TypeBox => "import { Type } from \"@sinclair/typebox\";".into(),
    };
    let header = format!(
        "// Generated by `osdl build --target {}`. Do not edit by hand.\n// Source of truth: the OSDL schema.\n{}\n",
        flavor.as_str(),
        import
    );

    let mut lines: Vec<String> = vec![header];
    if let Some(doc) = ast.model_doc(&model.name) {
        lines.push("/**".into());
        for line in doc.lines() {
            lines.push(format!(" * {line}"));
        }
        lines.push(" */".into());
    }

    let mut fields: Vec<&Field> = model.fields().map(|(_, f)| f).collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));

    // Build the field entries for the object schema.
    let mut entries: Vec<String> = Vec::new();
    for f in &fields {
        let doc_block = field_doc_lines(flavor, f, ast, &model.name);
        let entry = match flavor {
            ValidatorFlavor::Zod => format!("  {}: {},", f.name, field_schema(flavor, f)),
            ValidatorFlavor::Valibot => format!("  {}: {},", f.name, field_schema(flavor, f)),
            ValidatorFlavor::TypeBox => format!("  {}: {},", f.name, field_schema(flavor, f)),
        };
        for d in doc_block {
            entries.push(d);
        }
        entries.push(entry);
    }

    match flavor {
        ValidatorFlavor::Zod => {
            lines.push(format!("export const {} = z.object({{", model.name));
            for e in entries {
                lines.push(e);
            }
            lines.push("});".into());
        }
        ValidatorFlavor::Valibot => {
            lines.push(format!("export const {} = v.object({{", model.name));
            for e in entries {
                lines.push(e);
            }
            lines.push("});".into());
        }
        ValidatorFlavor::TypeBox => {
            lines.push(format!("export const {} = Type.Object({{", model.name));
            for e in entries {
                lines.push(e);
            }
            lines.push("});".into());
        }
    }
    lines.push(String::new());
    lines.join("\n")
}

fn render_barrel(flavor: ValidatorFlavor, names: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "// Generated by `osdl build --target {}`. Do not edit by hand.\n",
        flavor.as_str()
    ));
    for n in names {
        lines.push(format!("export {{ {n} }} from \"./{n}\";", n = n));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// `ModelName` -> `model_name` (snake_case).
fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.char_indices() {
        if i != 0 && c.is_uppercase() {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_parser::parse;

    fn render(flavor: ValidatorFlavor, src: &str) -> String {
        let ast = parse(src).expect("parse");
        osdl_core::Validator::validate(&ast, Some(Target::Zod)).expect("validate");
        let target = match flavor {
            ValidatorFlavor::Zod => Target::Zod,
            ValidatorFlavor::Valibot => Target::Valibot,
            ValidatorFlavor::TypeBox => Target::TypeBox,
        };
        let r = TsValidatorRenderer::new(target, flavor);
        let files = r.render(&ast).unwrap();
        let mut out = String::new();
        for (_, c) in files {
            out.push_str(&c);
        }
        out
    }

    #[test]
    fn zod_emits_object_schema() {
        let src =
            "User\n  id uuid -pk\n  email string -uniq\n  age int -null -check \"age >= 0\"\n";
        let out = render(ValidatorFlavor::Zod, src);
        assert!(out.contains("import { z } from \"zod\";"));
        assert!(out.contains("export const User = z.object({"));
        assert!(out.contains("id: z.string().uuid(),"));
        assert!(out.contains("age: z.number().int().nullable(),"));
        assert!(out.contains("check: age >= 0"));
    }

    #[test]
    fn valibot_emits_object_schema() {
        let src = "User\n  id uuid -pk\n  email string -uniq\n";
        let out = render(ValidatorFlavor::Valibot, src);
        assert!(out.contains("import * as v from \"valibot\";"));
        assert!(out.contains("export const User = v.object({"));
        assert!(out.contains("id: v.string(),"));
        assert!(out.contains("email: v.string() /* unique */,"));
    }

    #[test]
    fn typebox_emits_object_schema() {
        let src = "User\n  id uuid -pk\n  age int -null\n";
        let out = render(ValidatorFlavor::TypeBox, src);
        assert!(out.contains("import { Type } from \"@sinclair/typebox\";"));
        assert!(out.contains("export const User = Type.Object({"));
        assert!(out.contains("id: Type.String({ format: \"uuid\" }),"));
        assert!(out.contains("age: Type.Optional(Type.Integer()),"));
    }

    #[test]
    fn polymorphic_expands_to_pair() {
        let src = "Comment\n  id uuid -pk\n  target -polymorphic Post,Video\n";
        let out = render(ValidatorFlavor::Zod, src);
        assert!(out.contains("target_type: z.string()"));
        assert!(out.contains("target_id: z.string()"));
    }

    #[test]
    fn enum_emits_closed_set() {
        let src = "Status\n  id uuid -pk\n  state string -enum active,inactive\n";
        let zod = render(ValidatorFlavor::Zod, src);
        assert!(zod.contains("state: z.enum([\"active\", \"inactive\"]),"));
        let valibot = render(ValidatorFlavor::Valibot, src);
        assert!(valibot.contains("state: v.enumType([\"active\", \"inactive\"]),"));
        let typebox = render(ValidatorFlavor::TypeBox, src);
        assert!(typebox.contains(
            "state: Type.Union([Type.Literal(\"active\"), Type.Literal(\"inactive\")]),"
        ));
    }

    #[test]
    fn doc_and_deprecation_carried() {
        let src = "/// A registered account holder.
User
  id uuid -pk
  /// The user's primary email.
  email string -uniq -deprecated \"use contactEmail\"
";
        let out = render(ValidatorFlavor::Zod, src);
        assert!(out.contains(" * A registered account holder."));
        assert!(out.contains(" * The user's primary email."));
        assert!(out.contains(" * @deprecated use contactEmail"));
    }
}
