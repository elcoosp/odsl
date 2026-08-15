# OSDL — the Open Schema Definition Language

OSDL is a minimal, indentation-based schema language that compiles to
backend-native code: **SeaORM 2.0** entities (SQLite / Postgres / MySQL),
**MongoDB** Serde structs + `$jsonSchema` validators, **TypeScript** interfaces,
**GraphQL** SDL, and **OpenAPI** documents. It also produces deterministic
lockfiles so schema migrations can be auto-diffed and reliably applied.

OSDL is the single source of truth for your schema — one file drives your
database, your API contract, and your frontend types.

## Workspace layout

| Crate | Responsibility |
|-------|----------------|
| `osdl-core` | Shared AST, types, validation, `CodeRenderer` trait, lockfile contract |
| `osdl-parser` | pest grammar + lexer + type inference + doc/deprecation capture |
| `osdl-codegen` | `syn`/`quote`/`prettyplease` codegen helpers |
| `osdl-codegen-seaorm` | SeaORM 2.0 dense-entity renderer (SQLite/Postgres/MySQL) |
| `osdl-codegen-mongo` | MongoDB Serde + `$jsonSchema` renderer |
| `osdl-codegen-typescript` | TypeScript interfaces + runtime validators |
| `osdl-codegen-graphql` | GraphQL SDL (types, inputs, queries, mutations) |
| `osdl-codegen-openapi` | OpenAPI 3 document renderer |
| `osdl-migrator` | AST diff engine + lockfile I/O |
| `osdl-adapter` | Live DB adapters: SeaORM (SQLite/Postgres/MySQL) + MongoDB |
| `osdl-lsp` | Language Server Protocol server (diagnostics, hover, go-to-def) |
| `osdl-mcp` | Model Context Protocol server for AI agents |
| `osdl-cli` | `osdl` binary: `init`, `build`, `migrate`, `fmt`, `pull`, `lsp`, `mcp` |

## The OSDL surface syntax

Indentation is the only structural punctuation. A top-level (un-indented)
line opens a model; every indented line adds a field.

```osdl
# comments start with '#'
use user                       # module import (unquoted, ::-separated paths)
use billing::invoice

# A documentation comment attaches to the next model or field.
/// A registered account holder.
User
  id uuid -pk                  # field: name, type, intent flag(s)
  email string -uniq
  created_at datetime -tz      # name inference: *created_at -> datetime + tz
  posts -relation Post        # has-many relation to Post
  avatar -null                 # nullable

# Custom types (value objects) expand inline at codegen time.
type Email = string -check "email ~ '^[^@]+@[^@]+$'"

Post
  id uuid -pk
  /// The author of the post.
  author User.id -ondelete setnull -onupdate restrict   # FK + referential actions
  title string
```

### Modules (`use`)

Schemas can be split across files and composed with `use` declarations. Paths
are unquoted and `::`-separated (Rust/zig style). `osdl build` resolves the
module graph into a single merged AST; the lockfile stores the SHA-256 of every
source file so the merge is reproducible.

```osdl
use user
use billing::invoice

Order
  id uuid -pk
  user User.id
  invoice Invoice.id
```

### Custom types (`type`)

`type Name = base -intents` declares a value object. It is validated inline
(expanded to its base scalar + constraints) and, in backends that support it,
can render as a newtype wrapper.

```osdl
type Email   = string -check "email ~ '^[^@]+@[^@]+$'"
type Money   = bigint -check "value >= 0"

User
  id uuid -pk
  email Email -uniq
  balance Money
```

### Rich foreign-key semantics

```osdl
Post
  author User.id -ondelete cascade -onupdate restrict
```

`-ondelete` / `-onupdate` accept `cascade | restrict | setnull | setdefault |
noaction`. They map to SeaORM `ForeignKeyAction` (and are advisory on Mongo).
If omitted, the migration defaults to `Cascade` to keep lockfile output stable.

### Documentation & deprecation

* `///` lines become Rust doc comments, TypeScript JSDoc, GraphQL
  `"""descriptions"""`, and OpenAPI `description` fields.
* `-deprecated "reason"` becomes `#[deprecated]` in Rust, `@deprecated` in
  GraphQL, and `deprecated: true` in OpenAPI.

### Types

`string` `int` `bigint` `float` `bool` `datetime` `date` `uuid` `json` `binary`

### Intent flags

| Flag | Meaning |
|------|---------|
| `-pk` | Primary key |
| `-partition` | NoSQL partition key (Mongo `_id` sharding) |
| `-uniq` | Unique constraint |
| `-null` | Nullable field |
| `-fulltext` | Full-text index |
| `-tz` | Store timezone-aware timestamp |
| `-auto` | Auto-incrementing integer key |
| `-relation Model` | Has-many relation to `Model` |
| `-enum` | Enum-typed field (inline variants) |
| `-default "v"` | Column default value |
| `-m2m Model` | Many-to-many bridge to `Model` |
| `-softdelete` | Soft-delete marker (timestamp) |
| `-check "expr"` | Check constraint expression |
| `-polymorphic A,B` | Polymorphic reference to one of `A`/`B` |
| `-ondelete action` | FK referential action on delete |
| `-onupdate action` | FK referential action on update |
| `-deprecated "reason"` | Mark field deprecated |

### Type inference (no explicit type)

* `*_id` / `*_at` / `*_time` / `created` / `updated` → `datetime`
* `id` → `uuid`
* known model name → reference to that model's `id`
* `*_id` otherwise → inferred reference (foreign key)

## CLI

```bash
# Scaffold a new schema.osdl + osdl.lock
osdl init

# Build backend code (--target selects the renderer)
osdl build --target seaorm-sqlite        # SeaORM entities (SQLite)
osdl build --target seaorm-postgres      # SeaORM entities (Postgres)
osdl build --target seaorm-mysql         # SeaORM entities (MySQL)
osdl build --target mongo                # Mongo structs + jsonSchema
osdl build --target typescript           # TS interfaces + runtime validators
osdl build --target graphql              # GraphQL SDL
osdl build --target openapi              # OpenAPI 3 document
osdl build --watch                       # rebuild on change

# Reverse-engineer a live database into schema.osdl
osdl pull --db-url postgres://localhost/app

# Reformat an .osdl file in place (or stdin -> stdout)
osdl fmt schema.osdl

# Migrations (diff the schema against osdl.lock)
osdl migrate plan                         # print the migration plan
osdl migrate plan --apply                # print plan and update osdl.lock
osdl migrate create --out migrations      # write migrations/<ts>_<slug>.sql
osdl migrate create --sea-orm             # ...a full sea-orm-migration crate (no raw SQL)
osdl migrate up --db-url sqlite:///app.db?mode=rwc     # apply to a live DB
osdl migrate up --db-url postgres://localhost/app       # ...or Postgres
osdl migrate up --db-url mongodb://localhost:27017/app # ...or MongoDB
osdl migrate down --db-url ...            # roll back to the desired state
osdl migrate status --db-url ...          # diff lockfile / DB / schema

# Editor + agent integrations
osdl lsp                                  # Language Server Protocol over stdio
osdl mcp                                  # Model Context Protocol server over stdio
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
  Postgres/MySQL, and MongoDB `createCollection`/`collMod` validators. Table
  and collection names are shared with the renderers via `osdl_core::naming`,
  so generated code and migrations always agree.
* **Migration files** — `osdl migrate create` renders the same plan to
  timestamped `migrations/<ts>_<slug>.sql` (with `up`/`down` sections). With
  `--sea-orm` it scaffolds a **full `sea-orm-migration` crate** under
  `migrations/` (Cargo.toml, `src/lib.rs` `Migrator`, `src/main.rs` CLI, and one
  `src/m<ts>_<slug>.rs` per diff) using pure SeaQuery builders
  (`Table::create()`, `schema::*` helpers, `ForeignKey::create()`) — no raw SQL,
  portable across SQLite/Postgres/MySQL. Accumulating diffs append new `m*.rs`
  files and re-list them in the `Migrator`.
* **Multi-target rendering** — the `CodeRenderer` trait is the only extension
  point; adding a backend means implementing one `render` method. Docs and
  deprecation flow through shared side-maps on the `Ast`, so every renderer
  emits the same semantics (Rust `#[doc]`/`#[deprecated]`, TS JSDoc, GraphQL
  `"""`/`@deprecated`, OpenAPI `description`/`deprecated: true`).
* **DRY/KISS** — the `CodeRenderer` trait is the single seam between the
  language and its backends.
