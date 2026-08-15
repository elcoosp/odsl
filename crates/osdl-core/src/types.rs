//! Primitive types and intent flags of the OSDL language.
//!
//! These enums are the vocabulary shared by the parser, validator and
//! renderers. Keeping them here (in the contract crate) means the rest of the
//! workspace never has to agree on string spellings.

use serde::{Deserialize, Serialize};

/// A primitive scalar type understood by every target backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScalarType {
    String,
    Int,
    BigInt,
    Float,
    Bool,
    DateTime,
    Date,
    Uuid,
    Json,
    Binary,
}

impl ScalarType {
    /// The canonical OSDL keyword for this type.
    pub fn as_keyword(self) -> &'static str {
        match self {
            ScalarType::String => "string",
            ScalarType::Int => "int",
            ScalarType::BigInt => "bigint",
            ScalarType::Float => "float",
            ScalarType::Bool => "bool",
            ScalarType::DateTime => "datetime",
            ScalarType::Date => "date",
            ScalarType::Uuid => "uuid",
            ScalarType::Json => "json",
            ScalarType::Binary => "binary",
        }
    }

    /// Parse an OSDL type keyword (case-insensitive).
    pub fn from_keyword(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "string" | "str" | "text" => Some(ScalarType::String),
            "int" | "integer" | "i32" => Some(ScalarType::Int),
            "bigint" | "i64" => Some(ScalarType::BigInt),
            "float" | "double" | "f64" => Some(ScalarType::Float),
            "bool" | "boolean" => Some(ScalarType::Bool),
            "datetime" | "timestamp" => Some(ScalarType::DateTime),
            "date" => Some(ScalarType::Date),
            "uuid" => Some(ScalarType::Uuid),
            "json" => Some(ScalarType::Json),
            "binary" | "bytes" | "blob" => Some(ScalarType::Binary),
            _ => None,
        }
    }
}

/// Referential action for a foreign-key column on delete / on update.
///
/// Zero-stringly-typed: the parser accepts the OSDL keywords
/// (`cascade`, `restrict`, `setnull`, `setdefault`, `noaction`) and stores this
/// enum; the SQL layer maps it onto the backend's `ForeignKeyAction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FkAction {
    Cascade,
    Restrict,
    SetNull,
    SetDefault,
    NoAction,
}

impl FkAction {
    /// The canonical OSDL keyword for this action.
    pub fn as_keyword(self) -> &'static str {
        match self {
            FkAction::Cascade => "cascade",
            FkAction::Restrict => "restrict",
            FkAction::SetNull => "setnull",
            FkAction::SetDefault => "setdefault",
            FkAction::NoAction => "noaction",
        }
    }

    /// Parse an OSDL action keyword (case-insensitive).
    pub fn from_keyword(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "cascade" => Some(FkAction::Cascade),
            "restrict" => Some(FkAction::Restrict),
            "setnull" | "set null" | "null" => Some(FkAction::SetNull),
            "setdefault" | "set default" | "default" => Some(FkAction::SetDefault),
            "noaction" | "no action" | "none" => Some(FkAction::NoAction),
            _ => None,
        }
    }

    /// The SeaORM `ForeignKeyAction` equivalent for SQL backends.
    pub fn to_sea_orm(self) -> &'static str {
        match self {
            FkAction::Cascade => "ForeignKeyAction::Cascade",
            FkAction::Restrict => "ForeignKeyAction::Restrict",
            FkAction::SetNull => "ForeignKeyAction::SetNull",
            FkAction::SetDefault => "ForeignKeyAction::SetDefault",
            FkAction::NoAction => "ForeignKeyAction::NoAction",
        }
    }
}

/// A reference to another model's field, expressed as `Model.field`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Reference {
    pub model: String,
    pub field: String,
}

impl std::fmt::Display for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.model, self.field)
    }
}

/// A field's declared type.
///
/// Either a primitive scalar or a reference to another model's key. References
/// are resolved by the validator into concrete foreign-key types.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FieldType {
    Scalar(ScalarType),
    Ref(Reference),
    /// Inferred-as-reference-but-unresolved at parse time (resolved later).
    InferredRef(String),
}

/// Intent flags are declarative modifiers that describe *desired behaviour*
/// rather than a concrete database implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Intent {
    /// Primary key (SQL) / document id.
    Pk,
    /// NoSQL partition key (MongoDB `_id` sharding).
    Partition,
    /// Unique constraint.
    Uniq,
    /// Nullable field.
    Null,
    /// Full-text search index.
    Fulltext,
    /// Secondary (non-unique) index.
    Index,
    /// Timezone-aware timestamp.
    Tz,
    /// Auto-incrementing integer key.
    Auto,
    /// Relationship to another model (1:N / N:1 helper).
    Relation,
    /// Closed set of string values (native enum).
    Enum,
    /// A database-side default value (`-default <value>`).
    Default,
    /// Many-to-many relationship (`-m2m <Target>`): the compiler auto-generates
    /// a junction table linking `Source` and `Target`.
    M2m,
    /// Computed / serialized-only field: present in the Rust struct but
    /// NOT mapped to a database column (`-virtual`).
    Virtual,
    /// Marks a nullable timestamp column as the soft-delete marker
    /// (`-softdelete`); the model is logically deleted by setting it.
    SoftDelete,
    /// Inline CHECK constraint expression (`-check "age >= 18"`). The raw
    /// boolean expression is stored on the field and rendered as SQL `CHECK`.
    Check,
    /// Polymorphic reference: a field that may point at one of several models
    /// (`-polymorphic Post,Video`). Rendered as a `(target_type, target_id)`
    /// pair rather than a single foreign key.
    Polymorphic,
    /// Referential action on deletion of the referenced row (`-ondelete
    /// <action>`). The concrete value lives on `Field::on_delete`.
    OnDelete,
    /// Referential action on update of the referenced key (`-onupdate
    /// <action>`). The concrete value lives on `Field::on_update`.
    OnUpdate,
}

impl Intent {
    pub fn as_keyword(self) -> &'static str {
        match self {
            Intent::Pk => "-pk",
            Intent::Partition => "-partition",
            Intent::Uniq => "-uniq",
            Intent::Null => "-null",
            Intent::Fulltext => "-fulltext",
            Intent::Index => "-index",
            Intent::Tz => "-tz",
            Intent::Auto => "-auto",
            Intent::Relation => "-relation",
            Intent::Enum => "-enum",
            Intent::Default => "-default",
            Intent::M2m => "-m2m",
            Intent::Virtual => "-virtual",
            Intent::SoftDelete => "-softdelete",
            Intent::Check => "-check",
            Intent::Polymorphic => "-polymorphic",
            Intent::OnDelete => "-ondelete",
            Intent::OnUpdate => "-onupdate",
        }
    }
}
