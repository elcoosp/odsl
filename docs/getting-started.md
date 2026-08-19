# Getting Started

This tutorial gets you from zero to a running schema and generated backend in
about five minutes.

## 1. Install

```sh
cargo install --path crates/odsl-cli
```

Verify it works:

```sh
odsl --help
```

## 2. Create a schema

```sh
mkdir my-app && cd my-app
odsl init schema.odsl
```

Edit `schema.odsl`:

```odsl
/// A user account.
User
  id uuid -pk
  email string -uniq
  name string
  created_at datetime -tz

/// A blog post authored by a user.
Post
  id int -pk -auto
  author User.id
  title string
  body text
  published bool -default false
```

## 3. Validate and format

```sh
odsl lint schema.odsl          # run the schema-quality linter
odsl fmt schema.odsl           # canonicalise the file in place
```

## 4. Generate code

```sh
# Rust entities (SeaORM) for Postgres
odsl build --target seaorm-postgres --out generated/

# TypeScript types + Zod validators
odsl build --target typescript --out generated/
odsl build --target zod --out generated/

# A tRPC router wired to a Prisma-style ctx.db
odsl build --target trpc --out generated/
```

## 5. Run a migration

```sh
# Apply directly against a database
export DB_URL=postgres://localhost/myapp
odsl migrate create schema.odsl --db-url $DB_URL
odsl migrate up --db-url $DB_URL

# Or verify the migration is correct on a throwaway DB first
odsl migrate test --db-url sqlite:///test.db
```

## 6. Use it from an agent (MCP)

Point any MCP-capable agent at the `odsl` binary:

```sh
odsl mcp
```

The server exposes `read_schema`, `validate_schema`, `format_schema`, `build`,
`lint`, and `migrate_preview` tools over JSON-RPC stdio.

## Next steps

- `docs/backend-matrix.md` — which intents each backend supports.
- `docs/migration-guide.md` — coming from Prisma / Drizzle / SQLx.
- `README.md` — the full command reference and target list.
