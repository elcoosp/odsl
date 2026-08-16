# Migration Guide (from other tools)

OSDL is a single schema source that generates code for many backends. If you are
coming from another tool, this guide maps its concepts onto OSDL.

## From Prisma

| Prisma                  | OSDL                                    |
|-------------------------|-----------------------------------------|
| `model User {}`         | `User` (model)                          |
| `id Int @id @default(autoincrement())` | `id int -pk -auto`          |
| `id String @id @default(uuid())`        | `id uuid -pk`               |
| `email String @unique`  | `email string -uniq`                   |
| `name String?`          | `name string -null`                     |
| `posts Post[]`          | `posts -relation Post`                 |
| `createdAt DateTime @default(now())`     | `created_at datetime -tz -default now` |
| generator / client      | `osdl build --target <backend>`         |

Round-trip help: `osdl convert --direction from-prisma schema.prisma schema.osdl`
imports a Prisma schema; `osdl convert --direction to-prisma` exports one. Defaults
such as `uuid()` and `autoincrement()` are preserved verbatim so the round-trip is
faithful.

## From Drizzle

Drizzle's TypeScript schema is close to OSDL's mental model:

```ts
// Drizzle
export const users = pgTable("users", {
  id: serial("id").primaryKey(),
  email: text("email").unique().notNull(),
});
```

```osdl
// OSDL
User
  id int -pk -auto
  email string -uniq -null
```

A `from-drizzle` converter is on the roadmap (Phase 3.5); today use
`osdl convert --direction from-prisma` as the closest import path, or author the
`.osdl` directly — it is intentionally compact.

## From SQLx / raw SQL

Start from your `CREATE TABLE` statements and translate each column to an OSDL
field. Then generate the typed client with `osdl build --target seaorm-postgres`
instead of hand-writing query structs. For an existing database you can also
reverse-engineer a starting schema with `osdl pull --db-url <url>`.

## Migrations

Once you have an `.osdl`, OSDL owns migrations for you:

```sh
osdl migrate create schema.osdl --db-url $DB_URL   # writes a timestamped .sql / SeaORM module
osdl migrate up --db-url $DB_URL                  # applies and records the lockfile
osdl migrate test --db-url sqlite:///test.db       # verify on a throwaway DB
```

The `osdl.lock` file is the contract: re-parsing the same schema always produces
the same lockfile, so auto-diffed migrations are deterministic.
