# ADR-003: Trait-Based Renderers (the `CodeRenderer` Seam)

- **Status:** Accepted
- **Context:** ODSL must target many backends (SeaORM Postgres/SQLite/MySQL,
  MongoDB, TypeScript, GraphQL, OpenAPI, JSON Schema, Zod/Valibot/TypeBox, tRPC,
  ERD, Prisma/Drizzle interop) without tangling target-specific logic into the
  core compiler.
- **Decision:** Define a single `trait CodeRenderer { fn render(&self, &Ast) ->
  Result<Vec<(Path, String)>> }` in `odsl-core`. Each backend is its own crate
  implementing the trait and consuming a *validated* `Ast`.
- **Consequences:**
  - `+` Contributors add a target by implementing one trait; the core stays
    decoupled from any ORM/driver API.
  - `+` All targets share the same validation pipeline, so semantic guarantees
    are uniform.
  - `-` The trait return type (`Vec<(path, contents)>`) pushes file-writing
    policy to the CLI.
