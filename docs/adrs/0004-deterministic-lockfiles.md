# ADR-004: Deterministic Lockfiles & Auto-Diff Migrations

- **Status:** Accepted
- **Context:** OSDL must generate auto-diffing migrations (BR-003,
  REQ-FUNC-005) without a human writing `ALTER TABLE` by hand, and must produce
  byte-for-byte identical output across runs (REQ-NFR-DET-001).
- **Decision:**
  - A `osdl.lock` file is a deterministic serialization of the last compiled
    `Ast` (stable field order, SHA256 checksum). It is the public contract:
    breaking its format requires a MAJOR version bump + migration guide.
  - `migrate create` diffs the current `Ast` against the lockfile's `Ast` and
    emits `MigrationOp`s; `migrate up`/`down` turn those ops into real DDL or
    Mongo commands.
  - Rollback (`down_sql`) sources the prior column/table definition from the
    *current* (pre-migration) lockfile, so `DropModel`/`DropField`/`AlterField`
    produce real inverse DDL rather than `None`.
- **Consequences:**
  - `+` Migrations are reproducible and reviewable as diffs.
  - `+` Down-migrations are non-destructive data-wise (seed data is never
    deleted on `down`).
  - `-` The lockfile format is frozen; any schema change to `Lockfile` must be
    backward-compatible or accompanied by a major-version migration guide.
