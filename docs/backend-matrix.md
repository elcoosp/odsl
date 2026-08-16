# Backend Support Matrix

This table shows which OSDL intents each backend supports. It is generated from
the `target_supports(target, intent, ty)` rules in `osdl-core::validator`.

Legend: ✅ native support · ⚠️ advisory / best-effort · ❌ not supported

| Intent        | SQLite | Postgres | MySQL | MongoDB | TS / GraphQL / OpenAPI / JSON Schema / Zod / Valibot / TypeBox / tRPC |
|---------------|:------:|:-------:|:-----:|:-------:|:----------------------------------------------------------------------:|
| `-pk`         | ✅ | ✅ | ✅ | ✅ | ✅ (annotation) |
| `-uniq`       | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-null`       | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-auto`       | ✅ | ✅ | ✅ | ❌ (Mongo has no auto-increment) | ✅ |
| `-tz`         | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-relation`   | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-index`      | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-enum`       | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-default`    | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-m2m`        | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-virtual`    | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-soft-delete`| ✅ | ✅ | ✅ | ✅ | ✅ |
| `-check`      | ✅ | ✅ | ✅ | ✅ | ✅ |
| `-polymorphic`| ✅ | ✅ | ✅ | ✅ | ✅ |
| `-fulltext`   | ✅ (FTS5) | ✅ (GIN) | ✅ (FULLTEXT) | ✅ (text index) | ✅ |
| `-partition`  | ❌ | ❌ (table-level DDL) | ❌ (table-level DDL) | ✅ | ✅ |
| `-on-delete` / `-on-update` | ✅ | ✅ | ✅ | ✅ (advisory) | ✅ |

## Notes

- **SQL backends** map intents to real DDL/constraints. `-partition` is a
  table-level concern and is intentionally not expressed as a field flag.
- **MongoDB** supports most intents natively but has no auto-increment; `-auto`
  is rejected by the validator for the `mongo` target.
- **Transpile targets** (TypeScript, GraphQL, OpenAPI, JSON Schema, Zod,
  Valibot, TypeBox, tRPC) describe *types and constraints only* — every intent
  is carried through as a documentation/validation annotation rather than
  executed DDL.
