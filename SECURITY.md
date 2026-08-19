# Security Policy

## Supported Versions

ODSL is pre-1.0. Security fixes are applied to the latest `main` and released in
the next minor. The `odsl.lock` format follows semver: a breaking change to the
lockfile format requires a MAJOR release.

## Reporting a Vulnerability

Please report security issues **privately** rather than opening a public issue.
Email the maintainers or use GitHub Security Advisories for this repository.
Include:

- A description of the vulnerability and its impact.
- Steps to reproduce (a minimal `.odsl` and command, if applicable).
- Suggested mitigation, if known.

We aim to acknowledge within 72 hours and provide a remediation plan within 14
days for confirmed issues.

## Known Security Posture

- **Identifier validation:** `sanitize_ident` validates identifiers before they
  are interpolated into SQL (e.g. SQLite PRAGMA). Keep this applied to every
  place SQL is built from schema names.
- **Parameterized queries:** generated migrations and runtime adapters should use
  bound parameters, not raw string interpolation. The `record_applied` path and
  any place that interpolates user/schema-derived values into SQL must be
  audited when changed.
- **Code generation:** the Prisma parser (`parse_prisma`) and all codegen targets
  must not allow a model/field name or default to escape into generated Rust or
  TypeScript (e.g. shell metacharacters, template breaks). Inputs are validated
  at parse/validate time; new parsers must preserve that guarantee.
- **CI:** a `cargo audit` step must run on every PR.

## Supply Chain

- Dependencies are pinned via `Cargo.lock`. Run `cargo audit` regularly.
- The MCP and LSP servers read only the schema files you point them at; they do
  not execute arbitrary code.
