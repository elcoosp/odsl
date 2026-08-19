# Changelog

All notable changes to ODSL are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/) and the project adheres to
[Semantic Versioning](https://semver.org/). The lockfile format (`odsl.lock`)
is part of the public contract: breaking it requires a MAJOR version bump and a
migration guide.

## [Unreleased]

### Added
- **MCP `lint` tool** — the `odsl-mcp` server now exposes a `lint` tool that runs
  the schema lint rules and returns structured findings (rule id, severity,
  message) so agents can fix them directly. (Previously advertised but missing.)
- **MCP `migrate_preview` tool** — a read-only tool that diffs a schema against an
  optional current lockfile and returns the migration plan plus up/down SQL, so an
  agent can safely review a proposed schema change before it is applied.
- **`schema_matches_strict`** on `Ast` — like `schema_matches` but also compares
  column type keywords, so a silent type change (e.g. `int` → `bigint`) surfaces
  as drift instead of being masked by the structural-only comparison. Wired into
  `odsl migrate test` as a non-fatal warning.
- New `odsl lint`, `odsl erd`, and `odsl migrate test` commands documented in the
  README, plus DBML ERD output (`--format dbml`).

### Fixed
- **Migration rollback gaps (data-loss risk).** `down_sql` no longer returns `None`
  for `DropModel`/`DropField` or comment-only guards for `AlterField`. The down of
  every op is now the real inverse DDL, sourced from the prior lockfile:
  - `DropModel` down recreates the table from the prior lockfile.
  - `DropField` down re-adds the column (including SQLite rebuild) from the prior lockfile.
  - `AlterField` down restores the prior column type / nullability / uniqueness
    (real `ALTER COLUMN` DDL on Postgres/MySQL; SQLite rebuild).

### Changed
- `LintRule::as_str()` is now public (stable kebab-case identifiers such as
  `missing-timestamps`), used by the MCP `lint` tool output.

## [0.1.0] - initial release
- Core parser, validator, and codegen crates (SeaORM, Mongo, TypeScript, GraphQL,
  OpenAPI, JSON Schema, Zod/Valibot/TypeBox, tRPC), live DB adapters, LSP, and MCP.
- Deterministic lockfiles and auto-diffed migrations.
