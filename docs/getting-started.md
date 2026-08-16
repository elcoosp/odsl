# Getting Started

This tutorial gets you from zero to a running schema and generated backend in
about five minutes.

## 1. Install

```sh
cargo install --path crates/osdl-cli
```

Verify it works:

```sh
osdl --help
```

## 2. Create a schema

```sh
mkdir my-app && cd my-app
osdl init schema.osdl
```

Edit `schema.osdl`:

```osdl
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
osdl lint schema.osdl          # run the schema-quality linter
osdl fmt schema.osdl           # canonicalise the file in place
```

## 4. Generate code

```sh
# Rust entities (SeaORM) for Postgres
osdl build --target seaorm-postgres --out generated/

# TypeScript types + Zod validators
osdl build --target typescript --out generated/
osdl build --target zod --out generated/

# A tRPC router wired to a Prisma-style ctx.db
osdl build --target trpc --out generated/
```

## 5. Run a migration

```sh
# Apply directly against a database
export DB_URL=postgres://localhost/myapp
osdl migrate create schema.osdl --db-url $DB_URL
osdl migrate up --db-url $DB_URL

# Or verify the migration is correct on a throwaway DB first
osdl migrate test --db-url sqlite:///test.db
```

## 6. Use it from an agent (MCP)

Point any MCP-capable agent at the `osdl` binary:

```sh
osdl mcp
```

The server exposes `read_schema`, `validate_schema`, `format_schema`, `build`,
`lint`, and `migrate_preview` tools over JSON-RPC stdio.

## Next steps

- `docs/backend-matrix.md` — which intents each backend supports.
- `docs/migration-guide.md` — coming from Prisma / Drizzle / SQLx.
- `README.md` — the full command reference and target list.
