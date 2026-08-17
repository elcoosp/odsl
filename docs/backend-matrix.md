# Backend Support Matrix

This document is the canonical reference for which OSDL intent flags each
code-generation target supports natively. It is generated from the
`target_supports` function in `crates/osdl-core/src/validator.rs` — if you change
support there, update this table to match.

Legend: ✅ supported natively · ❌ not supported (compiler fails fast with a
`TargetIncompatibility` error) · ⚠️ supported as a documentation/constraint
annotation only (transpile targets emit the intent but do not create a database
object).

## Intent support by target

| Intent | seaorm-sqlite | seaorm-postgres | seaorm-mysql | mongo | typescript | graphql | openapi | json-schema | zod | valibot | typebox | trpc |
|--------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| `-pk` (primary key) | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-partition` (partition key) | ❌ | ❌ | ❌ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-uniq` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-null` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-auto` (auto-increment) | ✅ | ✅ | ✅ | ❌ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-tz` (timezone-aware datetime) | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-relation` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-hasone` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-index` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-enum` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-default` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-m2m` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-virtual` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-softdelete` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-check` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-polymorphic` | ✅ | ✅ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-fulltext` | ✅ (FTS5) | ✅ (GIN) | ✅ (FULLTEXT) | ✅ (text index) | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |
| `-ondelete` / `-onupdate` | ✅ | ✅ | ✅ | ✅ (advisory) | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ |

## Notes

- **`-partition` on SQL backends** is rejected because partition keys require
  table-level DDL (`PARTITION BY`) that the per-field intent flag cannot express.
  Use a composite `-pk` or model partitioning at the database layer instead.
- **`-auto` on Mongo** is rejected: MongoDB has no auto-increment. Use a `uuid`
  `-pk` instead.
- **Transpile targets** (TypeScript, GraphQL, OpenAPI, JSON Schema, Zod,
  Valibot, TypeBox, tRPC) describe types only. Every intent is emitted as a
  constraint/annotation in the generated artifact but never creates a database
  object — there is no database to support or reject it.
- The matrix is enforced at **build time** via `Validator::validate(ast,
  Some(target))`, which fails fast with a `TargetIncompatibility` error listing
  the unsupported feature, the target, and the offending field.

## Renderer crates

| Target | Crate |
|--------|-------|
| seaorm-sqlite / -postgres / -mysql | `osdl-codegen-seaorm` |
| mongo | `osdl-codegen-mongo` |
| typescript | `osdl-codegen-typescript` |
| graphql | `osdl-codegen-graphql` |
| openapi | `osdl-codegen-openapi` |
| json-schema | `osdl-codegen-jsonschema` |
| zod / valibot / typebox | `osdl-codegen-ts-validators` |
| trpc | `osdl-codegen-trpc` |
| erd (mermaid / dbml) | `osdl-codegen-erd` |
| prisma / drizzle (interop) | `osdl-codegen-prisma` / `osdl-codegen-drizzle` |
