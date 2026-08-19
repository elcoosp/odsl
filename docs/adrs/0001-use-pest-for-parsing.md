# ADR-001: Use `pest` (PEG) for Parsing

- **Status:** Accepted
- **Context:** ODSL is an indentation-based DSL with no curly braces or
  semicolons. We need a parser that is easy to read, easy to extend, and that
  fails gracefully on malformed input (never panics).
- **Decision:** Parse with the `pest` PEG crate. The grammar lives in
  `crates/odsl-parser/src/grammar.pest`; indentation is captured explicitly by an
  `indent` rule so it is not swallowed as implicit whitespace.
- **Consequences:**
  - `+` Declarative grammar is easy to read and modify.
  - `+` `pest`'s `Pair` stream makes the AST build straightforward.
  - `-` Whitespace/indentation rules need careful management in the grammar.
  - `-` PEG backtracking is not free; very large files rely on the workspace's
    `criterion` benchmark to keep parse time bounded.
