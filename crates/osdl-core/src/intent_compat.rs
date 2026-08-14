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
    /// MongoDB Serde structs + `$jsonSchema` validators.
    Mongo,
}

impl Target {
    /// The stable string used on the CLI and in messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Target::SeaOrmSqlite => "seaorm-sqlite",
            Target::SeaOrmPostgres => "seaorm-postgres",
            Target::Mongo => "mongo",
        }
    }

    /// The logical family: SQL backends share relation semantics.
    pub fn is_sql(self) -> bool {
        matches!(self, Target::SeaOrmSqlite | Target::SeaOrmPostgres)
    }
}

#[allow(clippy::should_implement_trait)]
impl FromStr for Target {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().replace(['_', '-'], "-").as_str() {
            "seaorm" | "seaorm-sqlite" | "sqlite" => Ok(Target::SeaOrmSqlite),
            "seaorm-postgres" | "postgres" | "pg" => Ok(Target::SeaOrmPostgres),
            "mongo" | "mongodb" => Ok(Target::Mongo),
            other => Err(format!("unknown target `{other}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr as _;
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
        assert!(<Target as FromStr>::from_str("oracle").is_err());
        assert_eq!(Target::Mongo.as_str(), "mongo");
    }
}
