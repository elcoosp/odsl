//! Integration tests for the OSDL parser, exercising the full parse pipeline
//! and the spec's BDD acceptance scenarios where applicable.

use osdl_core::ast::Ast;
use osdl_core::errors::{CompileErrorKind, OsdlError};
use osdl_core::types::{FieldType, Intent, ScalarType};
use osdl_parser::parse;
use osdl_parser::infer;

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
    let src = "User\n  id uuid -pk\n  posts -relation Post\nPost\n  id uuid -pk\n  author User.id\n";
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
    let err = osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
    match err {
        OsdlError::Compile { kind, .. } => assert_eq!(
            kind,
            CompileErrorKind::CyclicDependency { models: vec!["A".into(), "B".into()] }
        ),
        other => panic!("expected cyclic dependency error, got {other:?}"),
    }
}

#[test]
fn type_mismatch_detected_by_validator() {
    let src = "User\n  id uuid -pk\n  age int -fulltext\n";
    let ast = parse(src).unwrap();
    let err = osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
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
    let err = osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
    match err {
        OsdlError::Compile { kind, .. } => assert!(matches!(kind, CompileErrorKind::TargetIncompatibility { .. })),
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
