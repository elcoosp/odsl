//! MongoDB adapter: turns migration ops into collection validators.
//!
//! MongoDB has no fixed schema, so "migrations" mean managing the
//! `$jsonSchema` collection validator via `createCollection` (initial) and
//! `collMod` (alter). Dropping a field is expressed with `collMod`'s
//! `$unset` operator on the validator path.

use mongodb::Database;
use mongodb::bson::{Document, doc};
use mongodb::options::{CreateCollectionOptions, ValidationAction, ValidationLevel};
use osdl_core::ast::LockModel;
use osdl_core::lockfile::Lockfile;
use osdl_migrator::MigrationOp;

use crate::AdapterError;
use crate::naming::collection_name;

/// Map an OSDL scalar keyword to its BSON type name.
fn bson_type(keyword: &str) -> &'static str {
    match keyword {
        "string" => "string",
        "int" => "int",
        "bigint" => "long",
        "float" => "double",
        "bool" => "bool",
        "datetime" => "date",
        "date" => "date",
        "uuid" => "string",
        "json" => "object",
        "binary" => "binData",
        _ => "string",
    }
}

/// Build the `$jsonSchema` validator document for a model projection.
pub fn build_validator(model: &LockModel) -> Document {
    let mut properties = Document::new();
    let mut required = Vec::new();

    for field in &model.fields {
        let mut prop = Document::new();
        // A reference resolves to the referenced key (uuid by default).
        let is_ref = field.ty.contains('.') && !field.ty.starts_with('-');
        let btype = if is_ref {
            "string"
        } else {
            bson_type(&field.ty)
        };
        prop.insert("bsonType", btype);

        let is_pk = field.intents.iter().any(|i| i == "-pk");
        let is_uniq = field.intents.iter().any(|i| i == "-uniq");
        let is_null = field.intents.iter().any(|i| i == "-null");
        if is_uniq {
            prop.insert("uniqueItems", false); // informational; enforced via index separately
            prop.insert("description", "unique");
        }
        if is_pk {
            prop.insert("description", "primary key");
        }
        properties.insert(&field.name, prop);

        if !is_null && !is_pk {
            required.push(field.name.clone());
        }
    }

    let mut schema = Document::new();
    schema.insert("bsonType", "object");
    schema.insert("required", required);
    schema.insert("properties", properties);

    let mut validator = Document::new();
    validator.insert("$jsonSchema", schema);
    validator
}

/// Commands to run for a single op. Returns the raw `run_command` documents
/// (or `create_collection` calls expressed as a tagged enum below).
#[derive(Debug)]
pub enum MongoOp {
    /// Create a collection with an initial validator.
    Create { name: String, validator: Document },
    /// Modify an existing collection's validator via `collMod`.
    CollMod { name: String, validator: Document },
    /// Remove a field from the validator via `collMod` `$unset`.
    Unset { name: String, field: String },
    /// Drop the collection entirely.
    Drop { name: String },
}

/// Translate a [`MigrationOp`] into the Mongo command(s) required.
pub fn op_to_mongo(op: &MigrationOp, target: &Lockfile) -> Vec<MongoOp> {
    match op {
        MigrationOp::CreateModel { model } => {
            if let Some(m) = target.model_by_name(model) {
                vec![MongoOp::Create {
                    name: collection_name(model),
                    validator: build_validator(m),
                }]
            } else {
                vec![]
            }
        }
        MigrationOp::DropModel { model } => vec![MongoOp::Drop {
            name: collection_name(model),
        }],
        MigrationOp::AddField { model, field, .. }
        | MigrationOp::AlterField { model, field, .. } => {
            if let Some(m) = target.model_by_name(model) {
                vec![MongoOp::CollMod {
                    name: collection_name(model),
                    validator: build_validator(m),
                }]
            } else {
                vec![MongoOp::Unset {
                    name: collection_name(model),
                    field: field.clone(),
                }]
            }
        }
        MigrationOp::DropField { model, field } => vec![MongoOp::Unset {
            name: collection_name(model),
            field: field.clone(),
        }],
    }
}

/// Translate a [`MigrationOp`] into the inverse (down) Mongo command(s),
/// reverting `target -> current`. The forward op was applied against `target`;
/// rolling it back means: drop collections `up` created, remove fields `up`
/// added, and re-emit the *prior* (`current`) validator for fields/models that
/// `up` altered or dropped (so the schema returns to `current`).
pub fn op_to_mongo_down(op: &MigrationOp, current: &Lockfile) -> Vec<MongoOp> {
    match op {
        MigrationOp::CreateModel { model } => vec![MongoOp::Drop {
            name: collection_name(model),
        }],
        MigrationOp::DropModel { model } => {
            if let Some(m) = current.model_by_name(model) {
                vec![MongoOp::Create {
                    name: collection_name(model),
                    validator: build_validator(m),
                }]
            } else {
                vec![]
            }
        }
        MigrationOp::AddField { model, field, .. } => vec![MongoOp::Unset {
            name: collection_name(model),
            field: field.clone(),
        }],
        MigrationOp::DropField { model, .. } => {
            if let Some(m) = current.model_by_name(model) {
                vec![MongoOp::CollMod {
                    name: collection_name(model),
                    validator: build_validator(m),
                }]
            } else {
                vec![]
            }
        }
        MigrationOp::AlterField { model, .. } => {
            if let Some(m) = current.model_by_name(model) {
                vec![MongoOp::CollMod {
                    name: collection_name(model),
                    validator: build_validator(m),
                }]
            } else {
                vec![]
            }
        }
    }
}

/// Execute the planned Mongo ops against `db`.
pub async fn apply_ops(db: &Database, ops: &[MongoOp]) -> Result<(), AdapterError> {
    for op in ops {
        match op {
            MongoOp::Create { name, validator } => {
                let opts = CreateCollectionOptions::builder()
                    .validator(Some(validator.clone()))
                    .validation_level(Some(ValidationLevel::Moderate))
                    .validation_action(Some(ValidationAction::Error))
                    .build();
                db.create_collection(name.clone())
                    .with_options(opts)
                    .await
                    .map_err(|e| AdapterError::Exec(e.to_string()))?;
                tracing::info!(collection = %name, "created collection with validator");
            }
            MongoOp::CollMod { name, validator } => {
                let cmd = doc! {
                    "collMod": name.clone(),
                    "validator": validator.clone(),
                    "validationLevel": "moderate",
                    "validationAction": "error",
                };
                db.run_command(cmd).await?;
                tracing::info!(collection = %name, "updated validator");
            }
            MongoOp::Unset { name, field } => {
                let cmd = doc! {
                    "collMod": name.clone(),
                    "$unset": {
                        format!("validator.$jsonSchema.properties.{field}"): "",
                    },
                };
                db.run_command(cmd).await?;
                tracing::info!(collection = %name, field = %field, "removed field from validator");
            }
            MongoOp::Drop { name } => {
                db.collection::<mongodb::bson::Document>(name)
                    .drop()
                    .await
                    .map_err(|e| AdapterError::Exec(e.to_string()))?;
                tracing::info!(collection = %name, "dropped collection");
            }
        }
    }
    Ok(())
}

#[allow(unused_imports)]
use crate::naming as _naming;

#[cfg(test)]
mod tests {
    use super::*;
    use osdl_core::lockfile::lock_field;

    fn user_model() -> LockModel {
        LockModel {
            name: "User".into(),
            fields: vec![
                lock_field("id", "uuid", &["-pk"]),
                lock_field("email", "string", &["-uniq"]),
                lock_field("age", "int", &["-null"]),
            ],
            indexes: vec![],
        }
    }

    #[test]
    fn validator_document_shape() {
        let v = build_validator(&user_model());
        // round-trips through JSON so we can assert structure
        let json = mongodb::bson::to_document(&v).unwrap();
        let schema = json.get_document("$jsonSchema").unwrap();
        assert_eq!(schema.get_str("bsonType").unwrap(), "object");
        let required = schema.get_array("required").unwrap();
        // id is pk (not required here since we treat pk as not-required-by-default? we skip pk from required)
        let names: Vec<_> = required.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(names.contains(&"email"));
        assert!(!names.contains(&"age")); // nullable fields are not required
        assert!(!names.contains(&"id")); // pk excluded from required
        let props = schema.get_document("properties").unwrap();
        assert_eq!(
            props
                .get_document("email")
                .unwrap()
                .get_str("bsonType")
                .unwrap(),
            "string"
        );
        assert_eq!(
            props
                .get_document("age")
                .unwrap()
                .get_str("bsonType")
                .unwrap(),
            "int"
        );
    }

    #[test]
    fn create_op_uses_collection_name() {
        let lf = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::CreateModel {
            model: "User".into(),
        };
        let mongo_ops = op_to_mongo(&op, &lf);
        assert_eq!(mongo_ops.len(), 1);
        match &mongo_ops[0] {
            MongoOp::Create { name, .. } => assert_eq!(name, "users"),
            other => panic!("expected Create, got {other:?}"),
        }
    }

    #[test]
    fn drop_field_uses_unset() {
        let lf = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::DropField {
            model: "User".into(),
            field: "age".into(),
        };
        let mongo_ops = op_to_mongo(&op, &lf);
        assert!(matches!(mongo_ops[0], MongoOp::Unset { .. }));
    }

    #[test]
    fn down_create_drops_collection() {
        // The forward op created `users`; rolling back must drop it.
        let lf = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::CreateModel {
            model: "User".into(),
        };
        let down = op_to_mongo_down(&op, &lf);
        assert!(matches!(&down[0], MongoOp::Drop { name } if name == "users"));
    }

    #[test]
    fn down_drop_model_recreates_collection() {
        // The forward op dropped `users`; rolling back must recreate it with the
        // prior validator (sourced from `current`).
        let lf = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::DropModel {
            model: "User".into(),
        };
        let down = op_to_mongo_down(&op, &lf);
        assert!(matches!(&down[0], MongoOp::Create { name, .. } if name == "users"));
    }

    #[test]
    fn down_add_field_uses_unset() {
        let lf = Lockfile {
            version: 1,
            checksum: String::new(),
            models: vec![user_model()],
        };
        let op = MigrationOp::AddField {
            model: "User".into(),
            field: "nick".into(),
            ty: "string".into(),
            nullable: true,
            uniq: false,
        };
        let down = op_to_mongo_down(&op, &lf);
        assert!(matches!(&down[0], MongoOp::Unset { field, .. } if field == "nick"));
    }
}
