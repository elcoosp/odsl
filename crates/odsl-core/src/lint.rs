//! Schema linting engine.
//!
//! `odsl lint` enforces a configurable set of schema-quality rules. Rules are
//! keyed by a stable [`LintRule`] id; each can be assigned a [`Severity`]
//! (`error`, `warn`, `info`, `off`) either by the built-in defaults or by a
//! `.odsl-lint.toml` config file.
//!
//! The linter is intentionally decoupled from the CLI: [`Linter::lint`] takes
//! a parsed [`Ast`](crate::Ast) and returns a `Vec<`[`LintFinding`]`>`, which the
//! CLI renders and turns into an exit code.

use crate::ast::Ast;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// The stable identifier of a built-in lint rule.
///
/// Serialized as a kebab-case string (e.g. `missing-created-at`) when read
/// from `.odsl-lint.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LintRule {
    /// Model names should be PascalCase / singular nouns.
    ModelNaming,
    /// Field names should be snake_case.
    FieldNaming,
    /// Foreign-key columns should carry an index (`-index` / `-uniq`).
    FkMissingIndex,
    /// Event/audit-ish models should declare `created_at` / `updated_at`.
    MissingTimestamps,
    /// Models should carry a `///` doc comment describing their purpose.
    MissingModelDoc,
    /// A model with no fields is almost certainly a mistake.
    EmptyModel,
}

impl LintRule {
    /// Human-readable description for docs / `--help`.
    pub fn description(&self) -> &'static str {
        match self {
            LintRule::ModelNaming => "Model names should be PascalCase and singular",
            LintRule::FieldNaming => "Field names should be snake_case",
            LintRule::FkMissingIndex => {
                "Foreign-key fields should declare an index (-index or -uniq)"
            }
            LintRule::MissingTimestamps => {
                "Models should declare created_at / updated_at timestamps"
            }
            LintRule::MissingModelDoc => "Models should carry a `///` doc comment",
            LintRule::EmptyModel => "Models should declare at least one field",
        }
    }

    /// Every built-in rule, in display order.
    pub fn all() -> &'static [LintRule] {
        &[
            LintRule::ModelNaming,
            LintRule::FieldNaming,
            LintRule::FkMissingIndex,
            LintRule::MissingTimestamps,
            LintRule::MissingModelDoc,
            LintRule::EmptyModel,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            LintRule::ModelNaming => "model-naming",
            LintRule::FieldNaming => "field-naming",
            LintRule::FkMissingIndex => "fk-missing-index",
            LintRule::MissingTimestamps => "missing-timestamps",
            LintRule::MissingModelDoc => "missing-model-doc",
            LintRule::EmptyModel => "empty-model",
        }
    }
}

/// How severe a finding is.
///
/// `Off` means the rule produces no findings (and is not rendered).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Off,
    Info,
    Warn,
    #[default]
    Error,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Off => "off",
            Severity::Info => "info",
            Severity::Warn => "warn",
            Severity::Error => "error",
        }
    }
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single lint finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LintFinding {
    pub rule: LintRule,
    pub severity: Severity,
    /// 1-indexed source line, if the finding maps to a concrete declaration.
    pub line: usize,
    pub model: Option<String>,
    pub field: Option<String>,
    pub message: String,
}

impl LintFinding {
    /// Stable, machine- and human-readable rendering, e.g.
    /// `error[model-naming]: UserProfile (line 3): Model names should be PascalCase and singular`.
    pub fn render(&self) -> String {
        let loc = match (self.model.as_ref(), self.field.as_ref()) {
            (Some(m), Some(f)) => format!("{m}.{f}"),
            (Some(m), None) => m.clone(),
            (None, Some(f)) => f.clone(),
            (None, None) => "-".to_string(),
        };
        let line = if self.line > 0 {
            format!(" (line {})", self.line)
        } else {
            String::new()
        };
        format!(
            "{}[{}]: {}: {}{}",
            self.severity,
            self.rule.as_str(),
            loc,
            self.message,
            line
        )
    }
}

/// Per-rule severity overrides, loaded from `.odsl-lint.toml`.
#[derive(Debug, Clone, Default, Deserialize)]
struct RuleOverrides {
    /// `rules.<rule-id> = "warn" | "error" | "info" | "off"`
    #[serde(default)]
    rules: HashMap<String, Severity>,
}

/// The effective configuration: a severity for every built-in rule.
#[derive(Debug, Clone)]
pub struct LintConfig {
    severities: HashMap<LintRule, Severity>,
}

impl Default for LintConfig {
    fn default() -> Self {
        // Sensible defaults: naming + FK index are warnings (opinionated but
        // non-fatal), the rest are errors.
        let mut severities = HashMap::new();
        severities.insert(LintRule::ModelNaming, Severity::Warn);
        severities.insert(LintRule::FieldNaming, Severity::Warn);
        severities.insert(LintRule::FkMissingIndex, Severity::Warn);
        severities.insert(LintRule::MissingTimestamps, Severity::Error);
        severities.insert(LintRule::MissingModelDoc, Severity::Warn);
        severities.insert(LintRule::EmptyModel, Severity::Error);
        Self { severities }
    }
}

impl LintConfig {
    /// The severity assigned to a rule (never `Off` here — `off` rules are
    /// simply dropped from the map by [`LintConfig::from_file`]).
    pub fn severity(&self, rule: LintRule) -> Severity {
        self.severities
            .get(&rule)
            .copied()
            .unwrap_or(Severity::Error)
    }

    /// Load config from a `.odsl-lint.toml` file. Unknown rule ids are ignored;
    /// `off` rules are removed from the effective set. Missing/invalid files
    /// fall back to [`LintConfig::default`].
    pub fn from_file(path: &Path) -> Self {
        let mut config = LintConfig::default();
        let Ok(text) = std::fs::read_to_string(path) else {
            return config;
        };
        let Ok(overrides): Result<RuleOverrides, _> = toml::from_str(&text) else {
            return config;
        };
        for rule in LintRule::all() {
            if let Some(sev) = overrides.rules.get(rule.as_str()) {
                if *sev == Severity::Off {
                    config.severities.remove(rule);
                } else {
                    config.severities.insert(*rule, *sev);
                }
            }
        }
        config
    }

    /// A config with every rule set `Off` (used to verify the engine is
    /// config-driven without depending on the default severities).
    pub fn empty() -> Self {
        Self {
            severities: HashMap::new(),
        }
    }

    /// Force a single rule's severity (test helper / future `--set` flag).
    pub fn with_severity(mut self, rule: LintRule, sev: Severity) -> Self {
        if sev == Severity::Off {
            self.severities.remove(&rule);
        } else {
            self.severities.insert(rule, sev);
        }
        self
    }
}

/// The schema linter.
pub struct Linter {
    config: LintConfig,
}

impl Linter {
    pub fn new(config: LintConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(LintConfig::default())
    }

    /// Run every enabled rule against the AST and return all findings.
    pub fn lint(&self, ast: &Ast) -> Vec<LintFinding> {
        let mut findings = Vec::new();
        for (_, model) in ast.models() {
            let model_line = model.line;
            let model_name = &model.name;

            if self.config.severities.contains_key(&LintRule::EmptyModel)
                && model.fields().count() == 0
            {
                findings.push(self.finding(
                    LintRule::EmptyModel,
                    model_line,
                    Some(model_name.clone()),
                    None,
                    "model declares no fields",
                ));
            }

            if self.config.severities.contains_key(&LintRule::ModelNaming)
                && !is_pascal_case(model_name)
            {
                findings.push(self.finding(
                    LintRule::ModelNaming,
                    model_line,
                    Some(model_name.clone()),
                    None,
                    LintRule::ModelNaming.description(),
                ));
            }

            if self
                .config
                .severities
                .contains_key(&LintRule::MissingModelDoc)
                && ast.model_doc(model_name).is_none()
            {
                findings.push(self.finding(
                    LintRule::MissingModelDoc,
                    model_line,
                    Some(model_name.clone()),
                    None,
                    "model is missing a `///` doc comment",
                ));
            }

            // When a `config` block declares the audit columns (`audit
            // created_at,updated_at`), the schema-level convention is the
            // authority and the per-model missing-timestamps nag is suppressed
            // (per the roadmap: config removes the boilerplate the linter
            // otherwise flags on every model). Without config, the classic
            // `created_at && updated_at` heuristic applies.
            let has_ts = if ast.config.audit_fields.is_empty() {
                has_audit_timestamps(model, &ast.config.audit_fields)
            } else {
                true
            };
            if self
                .config
                .severities
                .contains_key(&LintRule::MissingTimestamps)
                && !has_ts
            {
                findings.push(self.finding(
                    LintRule::MissingTimestamps,
                    model_line,
                    Some(model_name.clone()),
                    None,
                    "model should declare created_at / updated_at",
                ));
            }

            for (_, field) in model.fields() {
                let field_name = &field.name;

                if self.config.severities.contains_key(&LintRule::FieldNaming)
                    && !is_snake_case(field_name)
                {
                    findings.push(self.finding(
                        LintRule::FieldNaming,
                        field.line,
                        Some(model_name.clone()),
                        Some(field_name.clone()),
                        LintRule::FieldNaming.description(),
                    ));
                }

                if self
                    .config
                    .severities
                    .contains_key(&LintRule::FkMissingIndex)
                    && is_foreign_key(field)
                    && !field.has(crate::types::Intent::Index)
                    && !field.has(crate::types::Intent::Uniq)
                {
                    findings.push(self.finding(
                        LintRule::FkMissingIndex,
                        field.line,
                        Some(model_name.clone()),
                        Some(field_name.clone()),
                        LintRule::FkMissingIndex.description(),
                    ));
                }
            }
        }
        findings
    }

    fn finding(
        &self,
        rule: LintRule,
        line: usize,
        model: Option<String>,
        field: Option<String>,
        message: &str,
    ) -> LintFinding {
        LintFinding {
            rule,
            severity: self.config.severity(rule),
            line,
            model,
            field,
            message: message.to_string(),
        }
    }
}

/// Returns true if `name` is PascalCase (starts uppercase, no underscores,
/// only ASCII alphanumerics).
fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    name.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Returns true if `name` is snake_case (lowercase, digits, underscores; does
/// not start or end with underscore).
fn is_snake_case(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if name.starts_with('_') || name.ends_with('_') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A foreign key is any reference-typed field (explicit or inferred) or any
/// field named like `*_id` / `*_fk`.
fn is_foreign_key(field: &crate::ast::Field) -> bool {
    use crate::types::FieldType;
    matches!(field.ty, FieldType::Ref(_) | FieldType::InferredRef(_))
        || field.name.ends_with("_id")
        || field.name.ends_with("_fk")
}

/// Heuristic: the model has both a `created_at` and an `updated_at` field.
/// Whether a model declares the schema's audit timestamp columns.
///
/// When `audit_fields` is non-empty (from a `config` block's `audit`
/// setting), the model satisfies the rule only if it declares *every* listed
/// audit column — this makes the `config` block the single source of truth for
/// the schema-wide convention, removing the boilerplate the linter otherwise
/// flags on every model. When `audit_fields` is empty, the classic
/// `created_at && updated_at` heuristic applies.
fn has_audit_timestamps(model: &crate::ast::Model, audit_fields: &[String]) -> bool {
    if audit_fields.is_empty() {
        let mut created = false;
        let mut updated = false;
        for (_, f) in model.fields() {
            if f.name == "created_at" {
                created = true;
            }
            if f.name == "updated_at" {
                updated = true;
            }
        }
        return created && updated;
    }
    audit_fields
        .iter()
        .all(|name| model.fields().any(|(_, f)| &f.name == name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Ast, Field, Model};
    use crate::types::{FieldType, Intent, ScalarType};

    fn field(name: &str, ty: FieldType, intents: Vec<Intent>, line: usize) -> Field {
        Field {
            name: name.to_string(),
            ty,
            intents,
            enum_variants: vec![],
            default_value: None,
            m2m_target: None,
            check_expr: None,
            polymorphic_targets: vec![],
            custom_type: None,
            on_delete: None,
            on_update: None,
            numeric_precision: None,
            numeric_scale: None,
            through_model: None,
            line,
        }
    }

    fn model(name: &str, fields: Vec<Field>, line: usize) -> Model {
        Model {
            name: name.to_string(),
            fields: {
                let mut arena = la_arena::Arena::new();
                for f in fields {
                    arena.alloc(f);
                }
                arena
            },
            field_index: Vec::new(),
            line,
            indexes: Vec::new(),
            primary_key: Vec::new(),
        }
    }

    fn build(ast: &mut Ast, m: Model) {
        ast.models.alloc(m);
    }

    #[test]
    fn detects_naming_and_fk_index_and_timestamps() {
        let mut ast = Ast::new();
        build(
            &mut ast,
            model(
                "user_profile", // snake_case model (bad)
                vec![
                    field(
                        "user_id",
                        FieldType::Ref(crate::types::Reference {
                            model: "User".into(),
                            field: "id".into(),
                        }),
                        vec![],
                        2,
                    ),
                    field(
                        "email",
                        FieldType::Scalar(ScalarType::String),
                        vec![Intent::Uniq],
                        3,
                    ),
                ],
                1,
            ),
        );
        let linter = Linter::with_defaults();
        let findings = linter.lint(&ast);
        let rules: Vec<_> = findings.iter().map(|f| f.rule).collect();
        assert!(rules.contains(&LintRule::ModelNaming));
        // FK without index/uniq -> warning.
        assert!(rules.contains(&LintRule::FkMissingIndex));
        // No created_at/updated_at -> error.
        assert!(rules.contains(&LintRule::MissingTimestamps));
        // Missing model doc -> warning.
        assert!(rules.contains(&LintRule::MissingModelDoc));
        // No field-naming or empty-model findings (fields are well-named, model non-empty).
        assert!(!rules.contains(&LintRule::FieldNaming));
        assert!(!rules.contains(&LintRule::EmptyModel));
    }

    #[test]
    fn clean_schema_has_no_findings() {
        let mut ast = Ast::new();
        build(
            &mut ast,
            model(
                "UserProfile",
                vec![
                    field(
                        "user_id",
                        FieldType::Ref(crate::types::Reference {
                            model: "User".into(),
                            field: "id".into(),
                        }),
                        vec![Intent::Index],
                        2,
                    ),
                    field(
                        "email",
                        FieldType::Scalar(ScalarType::String),
                        vec![Intent::Uniq],
                        3,
                    ),
                    field(
                        "created_at",
                        FieldType::Scalar(ScalarType::DateTime),
                        vec![],
                        4,
                    ),
                    field(
                        "updated_at",
                        FieldType::Scalar(ScalarType::DateTime),
                        vec![],
                        5,
                    ),
                ],
                1,
            ),
        );
        ast.model_docs
            .insert("UserProfile".into(), "An account.".into());
        let linter = Linter::with_defaults();
        assert!(
            linter.lint(&ast).is_empty(),
            "clean schema should lint clean"
        );
    }

    #[test]
    fn config_can_disable_rules() {
        let mut ast = Ast::new();
        build(
            &mut ast,
            model(
                "user_profile",
                vec![field(
                    "user_id",
                    FieldType::Ref(crate::types::Reference {
                        model: "User".into(),
                        field: "id".into(),
                    }),
                    vec![],
                    2,
                )],
                1,
            ),
        );
        // Disable everything: empty config => no findings.
        let linter = Linter::new(LintConfig::empty());
        assert!(linter.lint(&ast).is_empty());

        // Disable only model-naming; FK + timestamps still fire.
        let cfg = LintConfig::default().with_severity(LintRule::ModelNaming, Severity::Off);
        let linter = Linter::new(cfg);
        let rules: Vec<_> = linter.lint(&ast).iter().map(|f| f.rule).collect();
        assert!(!rules.contains(&LintRule::ModelNaming));
        assert!(rules.contains(&LintRule::FkMissingIndex));
    }

    #[test]
    fn empty_model_finding() {
        let mut ast = Ast::new();
        build(&mut ast, model("EmptyThing", vec![], 1));
        let linter = Linter::with_defaults();
        let findings = linter.lint(&ast);
        assert!(findings.iter().any(|f| f.rule == LintRule::EmptyModel));
    }
}
