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
| `osdl-cli` | `osdl` binary: `init`, `build`, `migrate` |

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
osdl migrate                           # print the migration plan
osdl migrate --apply                    # print plan and update osdl.lock
```

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
* **DRY/KISS** — the `CodeRenderer` trait is the only extension point; adding
  a backend means implementing one `render` method.
