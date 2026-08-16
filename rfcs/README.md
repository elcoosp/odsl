# RFC Process

Major changes to OSDL — new syntax, new backends, lockfile-format changes, or
anything that affects the public contract — go through the RFC process before
implementation.

## Why

The `CodeRenderer` trait is the single seam for codegen and the lockfile format
is the migration contract. Changes there have wide blast radius, so they deserve
written design review first.

## How to propose an RFC

1. Copy `rfcs/TEMPLATE.md` to `rfcs/XXXX-short-title.md` (XXXX = a zero-padded
   sequential number).
2. Fill in the sections: **Context**, **Decision**, **Alternatives**,
   **Consequences**, **Migration / Compatibility**.
3. Open a PR with the RFC. Discussion happens on the PR.
4. Once accepted, reference the RFC number in the implementing commit(s) and in
   the changelog.

## Current RFCs

- _(none yet — first RFCs: composite primary keys, view/materialized-view
  support, multi-file schemas, soft-delete config block.)_
