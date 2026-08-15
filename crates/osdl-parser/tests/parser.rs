//! Integration tests for the OSDL parser, exercising the full parse pipeline
//! and the spec's BDD acceptance scenarios where applicable.

use osdl_core::ast::Ast;
use osdl_core::errors::{CompileErrorKind, OsdlError};
use osdl_core::types::{FieldType, FkAction, Intent, ScalarType};
use osdl_parser::infer;
use osdl_parser::parse;
use proptest::prelude::*;

fn find_model<'a>(ast: &'a Ast, name: &str) -> &'a osdl_core::ast::Model {
    let idx = ast.model_by_name(name).expect("model should exist");
    &ast.models[idx]
}

fn find_field<'a>(model: &'a osdl_core::ast::Model, name: &str) -> &'a osdl_core::ast::Field {
    let idx = model.field_by_name(name).expect("field should exist");
    &model.fields[idx]
}

#[test]
fn parses_simple_model() {
    let src = "User\n  id uuid -pk\n  email string -uniq\n  created_at datetime -tz\n";
    let ast = parse(src).unwrap();
    let user = find_model(&ast, "User");
    let id = find_field(user, "id");
    assert_eq!(id.ty, FieldType::Scalar(ScalarType::Uuid));
    assert!(id.has(Intent::Pk));

    let email = find_field(user, "email");
    assert_eq!(email.ty, FieldType::Scalar(ScalarType::String));
    assert!(email.has(Intent::Uniq));

    let created = find_field(user, "created_at");
    assert!(created.has(Intent::Tz));
}

#[test]
fn infers_foreign_key_and_datetime() {
    let src = "User\n  id uuid -pk\nPost\n  id uuid -pk\n  user_id\n  posted_at\n";
    let ast = parse(src).unwrap();
    let post = find_model(&ast, "Post");
    let uid = find_field(post, "user_id");
    assert!(matches!(&uid.ty, FieldType::Ref(r) if r.model == "User" && r.field == "id"));
    let posted = find_field(post, "posted_at");
    assert_eq!(posted.ty, FieldType::Scalar(ScalarType::DateTime));
}

#[test]
fn resolves_explicit_reference() {
    let src = "User\n  id uuid -pk\nPost\n  id uuid -pk\n  author User.id\n";
    let ast = parse(src).unwrap();
    let post = find_model(&ast, "Post");
    let author = find_field(post, "author");
    assert!(matches!(&author.ty, FieldType::Ref(r) if r.model == "User" && r.field == "id"));
}

#[test]
fn resolves_relation_flag() {
    let src =
        "User\n  id uuid -pk\n  posts -relation Post\nPost\n  id uuid -pk\n  author User.id\n";
    let ast = parse(src).unwrap();
    let user = find_model(&ast, "User");
    let posts = find_field(user, "posts");
    assert!(posts.has(Intent::Relation));
}

#[test]
fn rejects_unknown_intent_flag() {
    let src = "User\n  id uuid -pk\n  bio string -frobnicate\n";
    let err = parse(src).unwrap_err();
    assert!(matches!(err, OsdlError::Parse(_)));
}

#[test]
fn cyclic_dependency_detected_by_validator() {
    // Parser succeeds; the validator must catch the cycle.
    let src = "A\n  id uuid -pk\n  b B.id\nB\n  id uuid -pk\n  a A.id\n";
    let ast = parse(src).unwrap();
    let err =
        osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
    match err {
        OsdlError::Compile { kind, .. } => assert_eq!(
            kind,
            CompileErrorKind::CyclicDependency {
                models: vec!["A".into(), "B".into()]
            }
        ),
        other => panic!("expected cyclic dependency error, got {other:?}"),
    }
}

#[test]
fn type_mismatch_detected_by_validator() {
    let src = "User\n  id uuid -pk\n  age int -fulltext\n";
    let ast = parse(src).unwrap();
    let err =
        osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
    match err {
        OsdlError::Compile { kind, .. } => assert_eq!(
            kind,
            CompileErrorKind::TypeMismatch {
                intent: "-fulltext".into(),
                ty: "int".into()
            }
        ),
        other => panic!("expected type mismatch error, got {other:?}"),
    }
}

#[test]
fn target_incompatibility_detected() {
    let src = "User\n  id uuid -partition\n";
    let ast = parse(src).unwrap();
    let err =
        osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
    match err {
        OsdlError::Compile { kind, .. } => assert!(matches!(
            kind,
            CompileErrorKind::TargetIncompatibility { .. }
        )),
        other => panic!("expected target incompatibility error, got {other:?}"),
    }
}

#[test]
fn inference_helper_works() {
    use std::collections::HashSet;
    let models: HashSet<String> = ["User".into()].into_iter().collect();
    assert!(matches!(
        infer::infer_field_type("user_id", &models, "Post"),
        FieldType::Ref(_)
    ));
}

// Property test: a model with a handful of well-formed scalar fields always
// parses into exactly one model with the expected field count.
proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(50))]

    #[test]
    fn parses_well_formed_model(
        fields in proptest::collection::vec(
            prop_oneof![
                "[a-z][a-z0-9_]*".prop_filter("need a name", |s| s != "id"),
                "email", "name", "age", "title", "body", "score", "active",
            ],
            1..6,
        ),
    ) {
        let unique: std::collections::HashSet<String> = fields.into_iter().collect();
        let mut src = String::from("User\n");
        src.push_str("  id uuid -pk\n");
        for f in &unique {
            src.push_str(&format!("  {f} string\n"));
        }
        let ast = parse(&src).expect("should parse");
        let idx = ast.model_by_name("User").expect("model present");
        let model = &ast.models[idx];
        prop_assert_eq!(model.fields().count(), unique.len() + 1);
    }
}

// --- Module system (`use`) ---

#[test]
fn records_use_declarations_single_file() {
    let src = "use user\nuse billing::invoice\nOrder\n  id uuid -pk\n  user User.id\n  invoice Invoice.id\n";
    let file = osdl_parser::parse_file(src).unwrap();
    assert_eq!(
        file.uses,
        vec!["user".to_string(), "billing::invoice".to_string()]
    );
    // The `use` lines are not modeled.
    assert!(file.ast.model_by_name("user").is_none());
    assert!(file.ast.model_by_name("Order").is_some());
}

#[test]
fn resolves_and_merges_modules_via_use() {
    let dir = std::env::temp_dir().join(format!("osdl-use-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let user_mod = dir.join("user.osdl");
    let billing = dir.join("billing");
    std::fs::create_dir_all(&billing).unwrap();
    let invoice = billing.join("invoice.osdl");

    std::fs::write(&user_mod, "User\n  id uuid -pk\n  email string -uniq\n").unwrap();
    std::fs::write(&invoice, "Invoice\n  id uuid -pk\n  total int\n").unwrap();
    std::fs::write(
        &dir.join("schema.osdl"),
        "use user\nuse billing::invoice\nOrder\n  id uuid -pk\n  user User.id\n  invoice Invoice.id\n  total int\n",
    )
    .unwrap();

    let project = osdl_parser::parse_project(&dir.join("schema.osdl")).unwrap();
    // All three models merged into one AST.
    assert!(project.ast.model_by_name("User").is_some());
    assert!(project.ast.model_by_name("Invoice").is_some());
    assert!(project.ast.model_by_name("Order").is_some());
    // References resolved across files.
    let order_idx = project.ast.model_by_name("Order").unwrap();
    let order = &project.ast.models[order_idx];
    let user_field = order.fields().find(|(_, f)| f.name == "user").unwrap().1;
    assert!(matches!(&user_field.ty, FieldType::Ref(r) if r.model == "User"));
    // Both imported files recorded as sources.
    assert_eq!(project.sources.len(), 3);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_detects_duplicate_model_across_files() {
    let dir = std::env::temp_dir().join(format!("osdl-use-dup-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(&dir.join("a.osdl"), "User\n  id uuid -pk\n").unwrap();
    std::fs::write(&dir.join("b.osdl"), "User\n  id uuid -pk\n").unwrap();
    std::fs::write(&dir.join("schema.osdl"), "use a\nuse b\n").unwrap();
    let err = osdl_parser::parse_project(&dir.join("schema.osdl")).unwrap_err();
    assert!(matches!(err, OsdlError::Parse(_)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_cycle_is_safe() {
    // A cycle of `use` must not infinite-loop; it resolves to a merged AST.
    let dir = std::env::temp_dir().join(format!("osdl-use-cycle-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(&dir.join("a.osdl"), "use b\nA\n  id uuid -pk\n").unwrap();
    std::fs::write(&dir.join("b.osdl"), "use a\nB\n  id uuid -pk\n").unwrap();
    std::fs::write(&dir.join("schema.osdl"), "use a\n").unwrap();
    let project = osdl_parser::parse_project(&dir.join("schema.osdl")).unwrap();
    assert!(project.ast.model_by_name("A").is_some());
    assert!(project.ast.model_by_name("B").is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

// --- Custom types / value objects (`type X = ...`) ---

#[test]
fn parses_custom_type_and_expands_field() {
    let src = "type Email = string -check \"email ~ '^[^@]+@[^@]+$'\"
type Money = bigint -check \"value >= 0\"
User
  id uuid -pk
  email Email -uniq
  balance Money
";
    let ast = parse(src).unwrap();
    assert!(ast.custom_type_by_name("Email").is_some());
    assert!(ast.custom_type_by_name("Money").is_some());
    let user = find_model(&ast, "User");
    let email = find_field(user, "email");
    assert_eq!(email.ty, FieldType::Scalar(ScalarType::String));
    assert!(email.has(Intent::Check));
    assert_eq!(email.check_expr.as_deref(), Some("email ~ '^[^@]+@[^@]+$'"));
    assert!(email.has(Intent::Uniq));
    assert_eq!(email.custom_type.as_deref(), Some("Email"));
    let balance = find_field(user, "balance");
    assert_eq!(balance.ty, FieldType::Scalar(ScalarType::BigInt));
    assert_eq!(balance.check_expr.as_deref(), Some("value >= 0"));
    assert_eq!(balance.custom_type.as_deref(), Some("Money"));
}

#[test]
fn rejects_custom_type_without_scalar_base() {
    let src = "type Broken = NotAScalar
User
  id uuid -pk
";
    let err = parse(src).unwrap_err();
    assert!(matches!(err, OsdlError::Parse(_)));
}

#[test]
fn parses_fk_referential_actions() {
    let src = "User
  id uuid -pk
Post
  id uuid -pk
  author User.id -ondelete setnull -onupdate restrict
";
    let ast = parse(src).unwrap();
    let post = find_model(&ast, "Post");
    let author = find_field(post, "author");
    assert_eq!(
        author.ty,
        FieldType::Ref(osdl_core::types::Reference {
            model: "User".into(),
            field: "id".into(),
        })
    );
    assert!(author.has(Intent::OnDelete));
    assert!(author.has(Intent::OnUpdate));
    assert_eq!(author.on_delete, Some(FkAction::SetNull));
    assert_eq!(author.on_update, Some(FkAction::Restrict));
}

#[test]
fn rejects_invalid_on_delete_action() {
    let src = "User
  id uuid -pk
Post
  id uuid -pk
  author User.id -ondelete bogus
";
    let err = parse(src).unwrap_err();
    assert!(matches!(err, OsdlError::Parse(_)));
}
