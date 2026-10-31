<div align="center">
  <img src="docs/logo.png" alt="ODSL Logo" width="300"/>
  <p>
    <strong>The Open Schema Definition Language</strong><br/>
    A minimal, indentation-based schema language that serves as the single source of truth for your database, API contract, and frontend types.
  </p>
  <p>
    <img src="https://img.shields.io/badge/Rust-1.94%20%7C%202024-000000?style=flat-square&logo=rust" alt="Rust"/>
    <img src="https://img.shields.io/badge/License-MIT-blue?style=flat-square" alt="License MIT"/>
    <img src="https://img.shields.io/badge/Targets-SeaORM%20%7C%20MongoDB%20%7C%20TS%20%7C%20GraphQL-262D3A?style=flat-square" alt="Targets"/>
    <img src="https://img.shields.io/badge/Build-Passing-brightgreen?style=flat-square&logo=githubactions" alt="Build"/>
  </p>
</div>

---

ODSL compiles to backend-native code, including **SeaORM 2.0** entities, **MongoDB** Serde structs + `$jsonSchema` validators, **TypeScript** interfaces (+ runtime validators), **GraphQL** SDL, **OpenAPI** documents, **JSON Schema**, **tRPC** routers, and **Entity-Relationship diagrams**. It also produces deterministic lockfiles for reliable auto-diffed schema migrations and can round-trip to **Prisma Schema Language**.

## Workspace Layout

ODSL is organized as a modular Cargo workspace for clarity, testability, and incremental development.

| Crate | Responsibility |
| :--- | :--- |
| `odsl-core` | Shared AST, types, validation, `CodeRenderer` trait, lockfile contract |
| `odsl-parser` | Pest grammar, lexer, type inference, doc/deprecation capture |
| `odsl-codegen` | `syn`/`quote`/`prettyplease` codegen helpers |
| `odsl-codegen-*` | Target-specific renderers (`seaorm`, `mongo`, `typescript`, `graphql`, `openapi`, `json-schema`, `ts-validators`, `trpc`, `erd`, `prisma`) |
| `odsl-migrator` | AST diff engine + lockfile I/O |
| `odsl-adapter` | Live DB adapters: SeaORM (SQLite/Postgres/MySQL) + MongoDB |
| `odsl-lsp` / `odsl-mcp` | Language Server Protocol & Model Context Protocol servers |
| `odsl-cli` | The `odsl` binary (`init`, `build`, `migrate`, `convert`, `fmt`, `lint`, `erd`, `pull`, `lsp`, `mcp`) |

## Surface Syntax

Indentation is the only structural punctuation. A top-level line opens a model; every indented line adds a field.

```odsl
# Comments start with '#'
use user                       # Module import (unquoted, ::-separated paths)
use billing::invoice

/// A registered account holder.
User
  id uuid -pk                  # Field: name, type, intent flag(s)
  email string -uniq
  created_at datetime -tz      # Name inference: *created_at -> datetime + tz
  posts -relation Post         # Has-many relation to Post
  avatar -null                 # Nullable field

# Custom types (value objects) expand inline at codegen time.
type Email = string -check "email ~ '^[^@]+@[^@]+$'"

Post
  id uuid -pk
  /// The author of the post.
  author User.id -ondelete setnull -onupdate restrict   # FK + referential actions
  title string
```

> [!TIP]
> **Type Inference**: ODSL infers types from naming conventions. `*_id`, `*_at`, `*_time`, `created`, and `updated` default to `datetime`. `id` defaults to `uuid`. Known model names infer foreign key references.

## Architecture & Design

- **Zero Stringly-Typed Codegen**: Every renderer builds a real `syn::File`, guaranteeing generated code parses correctly (verified against `sea-orm` 2.0).
- **Deterministic Lockfiles**: Structurally equal schemas produce byte-identical `odsl.lock` files (sorted projection + SHA-256), making auto-migrations highly reliable.
- **Live Execution**: `odsl-adapter` translates the backend-agnostic `MigrationPlan` into real DDL, ensuring generated code and migrations always agree on naming.
- **Multi-Target Rendering**: The `CodeRenderer` trait is the single extension point. Adding a backend requires implementing just one `render` method.

## CLI Usage

### Setup & Build
```bash
# Scaffold a new schema.odsl + odsl.lock
odsl init

# Build backend code (--target selects the renderer)
odsl build --target seaorm-postgres    # SeaORM entities (Postgres)
odsl build --target mongo              # Mongo structs + jsonSchema
odsl build --target typescript         # TS interfaces + runtime validators
odsl build --target graphql            # GraphQL SDL
odsl build --target openapi            # OpenAPI 3 document
odsl build --target trpc               # tRPC v10 routers (per-model CRUD)
odsl build --watch                     # Rebuild on change
```

### Migrations
```bash
odsl migrate plan                      # Print the migration plan
odsl migrate create --out migrations   # Write timestamped .sql or --sea-orm Rust modules
odsl migrate up --db-url postgres://localhost/app  # Apply to a live DB
odsl migrate test --db-url sqlite:///app.db        # Verify on a throwaway DB
```

### Interop & Tooling
```bash
# Round-trip with Prisma Schema Language
odsl convert --direction to-prisma schema.odsl schema.prisma
odsl convert --direction from-prisma schema.prisma schema.odsl

# Reverse-engineer a live database
odsl pull --db-url postgres://localhost/app

# Lint, format, and visualize
odsl lint schema.odsl
odsl fmt schema.odsl
odsl erd schema.odsl --format mermaid

# Editor & AI agent integrations
odsl lsp
odsl mcp
```

> [!NOTE]
> `migrate create` renders the schema diff into timestamped `.sql` files (with `up`/`down` sections) or a full `sea-orm-migration` crate using pure SeaQuery builders. Re-running `migrate up` is a no-op once the lockfile matches the schema.

## Building & Testing

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace
cargo fmt   --all
```

## Documentation

Explore the project further through our dedicated documentation:

- 📖 **[Getting Started](docs/getting-started.md)**: Five-minute tutorial (install, schema, build, migrate, MCP).
- 🗄️ **[Backend Matrix](docs/backend-matrix.md)**: Intent support across SQLite, Postgres, MySQL, Mongo, and transpilation targets.
- 🔄 **[Migration Guide](docs/migration-guide.md)**: Transitioning from Prisma, Drizzle, or SQLx.
- 📜 **[Language Spec](docs/spec.md)**: Complete syntax and semantics reference.
- 🏛️ **[Architecture Decisions](adr/)**: Design rationale (e.g., deterministic lockfiles as the migration contract).

---

> [!IMPORTANT]
> For vulnerability reporting, please refer to [SECURITY.md](SECURITY.md). Release notes and the lockfile format semver contract are tracked in [CHANGELOG.md](CHANGELOG.md).
