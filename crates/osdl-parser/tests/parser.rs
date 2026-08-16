//! Integration tests for the OSDL parser, exercising the full parse pipeline
//! and the spec's BDD acceptance scenarios where applicable.

use osdl_core::ast::Ast;
use osdl_core::errors::{CompileErrorKind, OsdlError};
use osdl_core::lockfile::Lockfile;
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
fn parses_numeric_precision_and_scale() {
    let src = "Product\n  id uuid -pk\n  price numeric -precision 18,4\n  qty numeric -precision 10\n  msrp numeric -scale 2\n";
    let ast = parse(src).unwrap();
    let product = find_model(&ast, "Product");
    let price = find_field(product, "price");
    assert_eq!(price.ty, FieldType::Scalar(ScalarType::Decimal));
    assert_eq!(price.numeric_precision, Some(18));
    assert_eq!(price.numeric_scale, Some(4));
    let qty = find_field(product, "qty");
    assert_eq!(qty.numeric_precision, Some(10));
    assert_eq!(qty.numeric_scale, None);
    let msrp = find_field(product, "msrp");
    assert_eq!(msrp.numeric_precision, None);
    assert_eq!(msrp.numeric_scale, Some(2));
}

#[test]
fn formulates_precision_roundtrip() {
    // `-precision p,s` must survive a format -> parse round-trip.
    use osdl_core::formatter::format_ast;
    let src = "Product\n  id uuid -pk\n  price numeric -precision 18,4\n";
    let ast = parse(src).unwrap();
    let formatted = format_ast(&ast);
    let reparsed = parse(&formatted).unwrap();
    let price = find_field(find_model(&reparsed, "Product"), "price");
    assert_eq!(price.numeric_precision, Some(18));
    assert_eq!(price.numeric_scale, Some(4));
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
        dir.join("schema.osdl"),
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
    std::fs::write(dir.join("a.osdl"), "User\n  id uuid -pk\n").unwrap();
    std::fs::write(dir.join("b.osdl"), "User\n  id uuid -pk\n").unwrap();
    std::fs::write(dir.join("schema.osdl"), "use a\nuse b\n").unwrap();
    let err = osdl_parser::parse_project(&dir.join("schema.osdl")).unwrap_err();
    assert!(matches!(err, OsdlError::Parse(_)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_cycle_is_safe() {
    // A cycle of `use` must not infinite-loop; it resolves to a merged AST.
    let dir = std::env::temp_dir().join(format!("osdl-use-cycle-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.osdl"), "use b\nA\n  id uuid -pk\n").unwrap();
    std::fs::write(dir.join("b.osdl"), "use a\nB\n  id uuid -pk\n").unwrap();
    std::fs::write(dir.join("schema.osdl"), "use a\n").unwrap();
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

#[test]
fn parses_model_and_field_doc_comments() {
    let src = "/// A registered account holder.
User
  /// The user's primary email address.
  email string -uniq
";
    let ast = parse(src).unwrap();
    let user = find_model(&ast, "User");
    assert_eq!(ast.model_doc("User"), Some("A registered account holder."));
    let _email = find_field(user, "email");
    assert_eq!(
        ast.field_doc("User", "email"),
        Some("The user's primary email address.")
    );
}

#[test]
fn parses_multi_line_doc_comment() {
    let src = "/// First line.
/// Second line.
User
  id uuid -pk
";
    let ast = parse(src).unwrap();
    assert_eq!(ast.model_doc("User"), Some("First line.\nSecond line."));
}

#[test]
fn parses_deprecated_field_directive() {
    let src = "User
  id uuid -pk
  email string -deprecated \"use contactEmail instead\"
  contactEmail string
";
    let ast = parse(src).unwrap();
    let user = find_model(&ast, "User");
    let _email = find_field(user, "email");
    assert_eq!(
        ast.field_deprecation("User", "email"),
        Some("use contactEmail instead")
    );
    // The replacement field is not deprecated.
    let _contact = find_field(user, "contactEmail");
    assert_eq!(ast.field_deprecation("User", "contactEmail"), None);
}

#[test]
fn doc_comments_do_not_alter_structure() {
    let src = "/// doc
User
  /// field doc
  id uuid -pk
  email string -uniq -deprecated \"x\"
";
    let ast = parse(src).unwrap();
    let user = find_model(&ast, "User");
    // No extra fields/fields created by docs.
    assert_eq!(user.fields.len(), 2);
    assert_eq!(ast.model_doc("User"), Some("doc"));
    assert_eq!(ast.field_doc("User", "id"), Some("field doc"));
}

#[test]
fn parses_composite_primary_key() {
    // A model-level `-pk a,b` declares a composite key; individual fields must
    // NOT also carry `-pk`.
    let src =
        "Membership\n  tenant_id uuid\n  user_id uuid\n  role string\n  -pk tenant_id,user_id\n";
    let ast = parse(src).unwrap();
    let m = find_model(&ast, "Membership");
    assert_eq!(
        m.primary_key,
        vec!["tenant_id".to_string(), "user_id".to_string()]
    );
    // Neither field carries the per-field `-pk` intent (model-level wins).
    assert!(!find_field(m, "tenant_id").has(Intent::Pk));
    assert!(!find_field(m, "user_id").has(Intent::Pk));
    // Derived key columns resolve correctly.
    assert_eq!(
        m.pk_columns(),
        vec!["tenant_id".to_string(), "user_id".to_string()]
    );
}

#[test]
fn rejects_mixed_composite_and_field_pk() {
    // Mixing `-pk a,b` with a per-field `-pk` is ambiguous and must fail.
    let src = "Membership\n  tenant_id uuid -pk\n  user_id uuid\n  -pk tenant_id,user_id\n";
    let ast = parse(src).unwrap();
    let err =
        osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
    assert!(matches!(
        err,
        OsdlError::Compile {
            kind: CompileErrorKind::InvalidKey { .. },
            ..
        }
    ));
}

#[test]
fn rejects_composite_pk_missing_column() {
    // A composite key referencing a non-existent column must fail.
    let src = "Membership\n  tenant_id uuid\n  -pk tenant_id,ghost\n";
    let ast = parse(src).unwrap();
    let err =
        osdl_core::Validator::validate(&ast, Some(osdl_core::Target::SeaOrmSqlite)).unwrap_err();
    assert!(matches!(
        err,
        OsdlError::Compile {
            kind: CompileErrorKind::InvalidKey { .. },
            ..
        }
    ));
}

#[test]
fn parses_index_options() {
    // Model-level index with Phase 1.2 options: -type, -prefix, -where, -order.
    let src = "Post
  id uuid -pk
  tenant_id uuid
  deleted_at datetime
  -index tenant_id,deleted_at -type gin -where \"deleted_at IS NULL\" -order desc
  -index tenant_id -prefix 10
  -uniq tenant_id,id -type btree
";
    let ast = parse(src).unwrap();
    let post = find_model(&ast, "Post");
    let gin = post
        .indexes
        .iter()
        .find(|i| i.fields == vec!["tenant_id".to_string(), "deleted_at".to_string()])
        .expect("gin index present");
    assert_eq!(gin.index_type.as_deref(), Some("gin"));
    assert_eq!(gin.where_clause.as_deref(), Some("deleted_at IS NULL"));
    assert_eq!(gin.order.as_deref(), Some("desc"));
    assert!(!gin.unique);
    let pref = post
        .indexes
        .iter()
        .find(|i| i.fields == vec!["tenant_id".to_string()])
        .expect("prefix index present");
    assert_eq!(pref.prefix_length, Some(10));
    let uniq = post
        .indexes
        .iter()
        .find(|i| i.unique)
        .expect("unique index present");
    assert_eq!(uniq.index_type.as_deref(), Some("btree"));
    assert_eq!(uniq.fields, vec!["tenant_id".to_string(), "id".to_string()]);
}

#[test]
fn parses_hasone_one_to_one() {
    let src = "User
  id uuid -pk
  profile Profile -hasone
Profile
  id uuid -pk
";
    let ast = parse(src).unwrap();
    let user = find_model(&ast, "User");
    let f = user
        .fields()
        .find(|(_, fl)| fl.name == "profile")
        .map(|(_, fl)| fl)
        .expect("profile field");
    assert!(f.has(Intent::HasOne));
    // The target resolves to the Profile model (a Ref here because Profile is a
    // known model), which relation_target()/renderers use for 1:1 wiring.
    assert!(matches!(f.ty, FieldType::Ref(ref r) if r.model == "Profile"));
}

#[test]
fn parses_through_uses_explicit_join() {
    let src = "Author
  id uuid -pk
  books Book -relation -through AuthorBook
Book
  id uuid -pk
AuthorBook
  id uuid -pk
  author Author.id
  book Book.id
";
    let ast = parse(src).unwrap();
    let author = find_model(&ast, "Author");
    let f = author
        .fields()
        .find(|(_, fl)| fl.name == "books")
        .map(|(_, fl)| fl)
        .expect("books field");
    assert_eq!(f.through_model.as_deref(), Some("AuthorBook"));
    // Lockfile expansion must NOT create an auto <Author>_<Book> junction
    // because -through names the join model explicitly.
    let lf = Lockfile::from_ast(&ast);
    assert!(lf.models.iter().any(|m| m.name == "AuthorBook"));
    assert!(!lf.models.iter().any(|m| m.name == "Author_Book"));
}

#[test]
fn parses_config_block() {
    let src = "config
  default-type uuid
  timestamp-format iso8601
  soft-delete field=deleted_at
  audit created_at,updated_at

User
  id uuid -pk
";
    let ast = parse(src).unwrap();
    let c = &ast.config;
    assert_eq!(c.default_type.as_deref(), Some("uuid"));
    assert_eq!(c.timestamp_format.as_deref(), Some("iso8601"));
    assert_eq!(c.soft_delete_field.as_deref(), Some("deleted_at"));
    assert_eq!(c.audit_fields, vec!["created_at", "updated_at"]);
    // The config block must not create a phantom "config" model.
    assert!(ast.model_by_name("config").is_none());
    // The real model is still parsed.
    assert!(ast.model_by_name("User").is_some());
}

#[test]
fn config_suppresses_missing_timestamps_lint() {
    use osdl_core::lint::{LintConfig, LintRule, Linter};
    let src = "config\n  audit created_at,updated_at\n\nUser\n  id uuid -pk\n";
    let ast = parse(src).unwrap();
    let linter = Linter::new(LintConfig::default());
    let findings = linter.lint(&ast);
    // User declares both audit columns per the config, so no missing-timestamps.
    let ts = findings
        .iter()
        .filter(|f| f.rule == LintRule::MissingTimestamps)
        .count();
    assert_eq!(ts, 0, "expected no missing-timestamps, got: {findings:?}");
}

// --- Phase 1.5: views (read-models) ---

#[test]
fn parses_plain_view() {
    let src = "view RecentPosts = SELECT p.id, p.title FROM posts p ORDER BY p.created_at DESC\n";
    let ast = parse(src).unwrap();
    assert_eq!(ast.views.len(), 1);
    let v = ast.view_by_name("RecentPosts").unwrap();
    assert!(!v.materialized);
    assert!(v.fields.is_empty());
    assert_eq!(
        v.query.trim(),
        "SELECT p.id, p.title FROM posts p ORDER BY p.created_at DESC"
    );
}

#[test]
fn parses_view_with_projection_and_materialized() {
    let src = "view UserSummary id uuid, email string -materialized = SELECT u.id, u.email FROM users u\n";
    let ast = parse(src).unwrap();
    let v = ast.view_by_name("UserSummary").unwrap();
    assert!(v.materialized);
    assert_eq!(v.fields.len(), 2);
    assert_eq!(v.fields[0].name, "id");
    assert_eq!(v.fields[0].ty, "uuid");
    assert_eq!(v.fields[1].name, "email");
    assert_eq!(v.fields[1].ty, "string");
}

#[test]
fn parses_multiline_view_query() {
    let src = "view ActiveUsers id uuid, email string -materialized =\n  SELECT u.id, u.email\n  FROM users u\n  WHERE u.active = true\n";
    let ast = parse(src).unwrap();
    let v = ast.view_by_name("ActiveUsers").unwrap();
    assert_eq!(v.fields.len(), 2);
    assert!(v.query.contains("WHERE u.active = true"));
    assert!(v.query.contains("FROM users u"));
}

#[test]
fn view_round_trips_through_format() {
    use osdl_core::formatter::format_ast;
    let src = "view UserSummary id uuid, email string -materialized = SELECT u.id, u.email FROM users u\n";
    let a = parse(src).unwrap();
    let formatted = format_ast(&a);
    let b = parse(&formatted).unwrap();
    assert_eq!(a.views.len(), b.views.len());
    let va = a.view_by_name("UserSummary").unwrap();
    let vb = b.view_by_name("UserSummary").unwrap();
    assert_eq!(va.name, vb.name);
    assert_eq!(va.materialized, vb.materialized);
    assert_eq!(va.fields, vb.fields);
    assert_eq!(va.query, vb.query);
}

#[test]
fn view_without_equals_is_rejected_at_parse_time() {
    // A view declaration must use `=` to separate the projection from the
    // query; otherwise it is a parse error (not a silent validation miss).
    let src = "view Broken\n";
    let err = parse(src).unwrap_err();
    assert!(format!("{err}").contains("must use `=`"));
}

#[test]
fn view_with_equals_but_empty_query_is_rejected_at_validation() {
    // `= ` with nothing after it parses (valid syntax) but validation rejects
    // the missing query body.
    let src = "view Broken = \n";
    let ast = parse(src);
    let ast = if let Ok(a) = ast {
        a
    } else {
        panic!("expected parse to succeed for `view Broken = `: {:?}", ast);
    };
    let err = osdl_core::validator::Validator::validate(&ast, None).unwrap_err();
    assert!(format!("{err}").contains("must declare a query"));
}

#[test]
fn view_name_collision_with_model_is_rejected() {
    let src = "User\n  id uuid -pk\n\nview User = SELECT * FROM users\n";
    let ast = parse(src).unwrap();
    let err = osdl_core::validator::Validator::validate(&ast, None).unwrap_err();
    assert!(format!("{err}").contains("collides with a model"));
}

#[test]
fn unknown_view_projection_type_is_rejected() {
    let src = "view V bad foo = SELECT 1\n";
    let ast = parse(src).unwrap();
    let err = osdl_core::validator::Validator::validate(&ast, None).unwrap_err();
    assert!(format!("{err}").contains("unknown type `foo`"));
}
