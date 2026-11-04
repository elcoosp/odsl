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
- **Modularity (internal).** The `odsl-cli` binary is split from a single
  1,600-line `main.rs` into focused modules (`project`, `build`, `scaffold`,
  `convert`, `migrate`, `quality`); the CLI argument model and dispatch stay in
  `main.rs`. No behavioural changes.
- **DRY (internal).** Identifier-naming helpers now live only in
  `odsl-core::naming`: `to_snake`, `to_snake_plural` and the newly added
  `to_pascal_case` (also re-exported by `odsl-codegen` for compatibility).
  Private duplicates in the SeaORM, MongoDB, TypeScript and tRPC renderers were
  removed. `Lockfile::empty()` replaces six hand-rolled empty-lockfile
  literals, and the CLI shares `load_project`, `confirm_destructive`,
  `history_entries` and `new_runtime` helpers instead of repeating the
  load/lockfile, destructive-guard, DB-history and runtime boilerplate. The
  Drizzle renderer deliberately keeps its camelCase-aware `to_snake`
  (documented). No behavioural changes.

## [0.1.0] - initial release
- Core parser, validator, and codegen crates (SeaORM, Mongo, TypeScript, GraphQL,
  OpenAPI, JSON Schema, Zod/Valibot/TypeBox, tRPC), live DB adapters, LSP, and MCP.
- Deterministic lockfiles and auto-diffed migrations.
