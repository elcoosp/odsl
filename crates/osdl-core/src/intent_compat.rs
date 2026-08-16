//! Target backend identifiers and command-line parsing for `--target`.

use clap::ValueEnum;
use std::str::FromStr;

/// A code-generation target backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, ValueEnum)]
pub enum Target {
    /// SeaORM entities for SQLite.
    SeaOrmSqlite,
    /// SeaORM entities for Postgres.
    SeaOrmPostgres,
    /// SeaORM entities for MySQL / MariaDB.
    SeaOrmMysql,
    /// MongoDB Serde structs + `$jsonSchema` validators.
    Mongo,
    /// TypeScript interfaces (single source of truth for the frontend).
    #[value(name = "typescript")]
    TypeScript,
    /// GraphQL schema (SDL types per model).
    #[value(name = "graphql")]
    GraphQl,
    /// OpenAPI 3 component schemas (API DTOs).
    #[value(name = "openapi")]
    OpenApi,
    /// JSON Schema (draft 2020-12) per model.
    #[value(name = "json-schema")]
    JsonSchema,
}

impl Target {
    /// The stable string used on the CLI and in messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Target::SeaOrmSqlite => "seaorm-sqlite",
            Target::SeaOrmPostgres => "seaorm-postgres",
            Target::SeaOrmMysql => "seaorm-mysql",
            Target::Mongo => "mongo",
            Target::TypeScript => "typescript",
            Target::GraphQl => "graphql",
            Target::OpenApi => "openapi",
            Target::JsonSchema => "json-schema",
        }
    }

    /// The logical family: SQL backends share relation semantics.
    pub fn is_sql(self) -> bool {
        matches!(
            self,
            Target::SeaOrmSqlite | Target::SeaOrmPostgres | Target::SeaOrmMysql
        )
    }

    /// Whether this target is a transpile-only target (no database backend).
    pub fn is_transpile(self) -> bool {
        matches!(
            self,
            Target::TypeScript | Target::GraphQl | Target::OpenApi | Target::JsonSchema
        )
    }
}

#[allow(clippy::should_implement_trait)]
impl FromStr for Target {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().replace(['_', '-'], "-").as_str() {
            "seaorm" | "seaorm-sqlite" | "sqlite" => Ok(Target::SeaOrmSqlite),
            "seaorm-postgres" | "postgres" | "pg" => Ok(Target::SeaOrmPostgres),
            "seaorm-mysql" | "mysql" | "mariadb" => Ok(Target::SeaOrmMysql),
            "mongo" | "mongodb" => Ok(Target::Mongo),
            "typescript" | "ts" => Ok(Target::TypeScript),
            "graphql" | "graphql-sdl" | "gql" => Ok(Target::GraphQl),
            "openapi" | "oas" | "swagger" => Ok(Target::OpenApi),
            "json-schema" | "jsonschema" | "json_schema" => Ok(Target::JsonSchema),
            other => Err(format!("unknown target `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_round_trip() {
        assert_eq!(
            <Target as FromStr>::from_str("mongo").unwrap(),
            Target::Mongo
        );
        assert_eq!(
            <Target as FromStr>::from_str("seaorm_postgres").unwrap(),
            Target::SeaOrmPostgres
        );
        assert_eq!(
            <Target as FromStr>::from_str("SQLITE").unwrap(),
            Target::SeaOrmSqlite
        );
        assert_eq!(
            <Target as FromStr>::from_str("mysql").unwrap(),
            Target::SeaOrmMysql
        );
        assert!(<Target as FromStr>::from_str("oracle").is_err());
        assert_eq!(Target::Mongo.as_str(), "mongo");
        assert_eq!(Target::SeaOrmMysql.as_str(), "seaorm-mysql");
    }
}
