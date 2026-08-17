# ADR-002: Use Arena Allocation for the AST

- **Status:** Accepted
- **Context:** OSDL models form cyclic graphs (e.g. `User` has `Post`, `Post`
  has `User`). Rust's ownership model makes a naive tree of owned references
  impossible to mutate and traverse.
- **Decision:** Represent the AST as an arena (`la-arena`) of `Model`/`Field`
  nodes referenced by stable indices, not by Rust references.
- **Consequences:**
  - `+` Bypasses strict borrow-checker rules for cyclic graphs.
  - `+` AST traversal and validation are far safer to write and free of
    lifetime annotations.
  - `-` Nodes are accessed via indices, which is slightly more verbose than
    field access.
