# OSDL — the Open Schema Definition Language

OSDL is a minimal, indentation-based schema language that compiles to
backend-native code: **SeaORM 2.0** entities (SQLite / Postgres) and
**MongoDB** Serde structs + `$jsonSchema` validators. It also produces
deterministic lockfiles so schema migrations can be auto-diffed.

## Workspace layout

| Crate | Responsibility |
|-------|----------------|
| `osdl-core` | Shared AST, types, validation, `CodeRenderer` trait, lockfile |
| `osdl-parser` | pest grammar + lexer + type inference |
| `osdl-codegen` | `syn`/`quote`/`prettyplease` codegen helpers |
| `osdl-codegen-seaorm` | SeaORM 2.0 dense-entity renderer |
| `osdl-codegen-mongo` | MongoDB Serde + `$jsonSchema` renderer |
| `osdl-migrator` | AST diff engine + lockfile I/O |
| `osdl-adapter` | Live DB adapters: SeaORM (SQLite/Postgres) + MongoDB |
| `osdl-cli` | `osdl` binary: `init`, `build`, `migrate`, `migrate up` |

## The OSDL surface syntax

Indentation is the only structural punctuation. A top-level (un-indented)
line opens a model; every indented line adds a field.

```osdl
# comments start with '#'
User                       # model declaration
  id uuid -pk              # field: name, type, intent flag(s)
  email string -uniq
  created_at datetime -tz   # name inference: *created_at -> datetime + tz
  posts -relation Post     # has-many relation to Post
Post
  id uuid -pk
  author User.id           # explicit foreign key (Model.field)
  title string
```

### Types
`string` `int` `bigint` `float` `bool` `datetime` `date` `uuid` `json` `binary`

### Intent flags
`-pk` `-partition` `-uniq` `-null` `-fulltext` `-tz` `-auto` `-relation Model`

### Type inference (no explicit type)
* `*_id` / `*_at` / `*_time` / `created` / `updated` → `datetime`
* `id` → `uuid`
* known model name → reference to that model's `id`
* `*_id` otherwise → inferred reference (foreign key)

## CLI

```bash
osdl init                              # scaffold schema.osdl + osdl.lock
osdl build --target sea-orm-sqlite     # emit SeaORM entities
osdl build --target mongo               # emit Mongo structs + jsonSchema
osdl migrate plan                      # print the migration plan
osdl migrate plan --apply              # print plan and update osdl.lock
osdl migrate create --out migrations   # write migrations/<ts>_<slug>.sql
osdl migrate create --sea-orm          # ...as SeaORM up/down Rust modules
osdl migrate up --db-url sqlite:///app.db?mode=rwc     # apply to a live DB
osdl migrate up --db-url postgres://localhost/app       # ...or Postgres
osdl migrate up --db-url mongodb://localhost:27017/app # ...or MongoDB
```

`migrate create` writes a migration file from the schema diff — a timestamped
`.sql` (with `up`/`down` sections) or, with `--sea-orm`, a `Migrator`
`up`/`down` Rust module. `migrate up` connects to the database named by
`--db-url`, applies every op in the diff (CREATE/ALTER/DROP for SQL;
`createCollection` / `collMod` validators for Mongo), then writes the new
`osdl.lock`. Re-running is a no-op once the lockfile matches the schema.

## Building & testing

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace
cargo fmt  --all
```

## Design notes

* **Zero stringly-typed codegen** — every renderer builds a real `syn::File`,
  so generated code is guaranteed to parse. (Verified by compiling the
  generated SeaORM output against `sea-orm` 2.0.)
* **Deterministic lockfiles** — two structurally-equal schemas produce
  byte-identical `osdl.lock` files (sorted projection + SHA-256), which is
  what makes auto-migration reliable.
* **Live execution** — `osdl-adapter` turns the backend-agnostic
  `MigrationPlan` into real DDL: SeaORM `execute_unprepared` for SQLite/
  Postgres, and MongoDB `createCollection`/`collMod` validators. Table and
  collection names are shared with the renderers via `osdl_core::naming`, so
  generated code and migrations always agree.
* **Migration files** — `osdl migrate create` renders the same plan to
  timestamped `migrations/<ts>_<slug>.sql` (with `up`/`down` sections) or,
  with `--sea-orm`, a SeaORM `Migrator` `up`/`down` Rust module, so the diff is
  also persisted as reviewable, replayable files (not just executed live).
* **DRY/KISS** — the `CodeRenderer` trait is the only extension point; adding
  a backend means implementing one `render` method.
