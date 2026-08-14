### The Workspace Layout

```text
osdl/
├── Cargo.toml           # Workspace root
├── crates/
│   ├── osdl-core/       # Agent 0: Shared AST & Traits (The Contract)
│   ├── osdl-parser/     # Agent 1: Pest Grammar & Lexer
│   ├── osdl-codegen/    # Agent 2: syn/quote Code Generation (Shared logic)
│   ├── osdl-codegen-seaorm/ # Agent 3: SeaORM 2.x Renderer
│   ├── osdl-codegen-mongo/  # Agent 4: MongoDB 3.x Renderer
│   ├── osdl-migrator/   # Agent 5: Diffing Engine & Lockfile
│   └── osdl-cli/        # Agent 6: CLI (clap), UX (miette), Wiring
```

### Subcrate Responsibilities & Parallel AI Agents

#### 1. `osdl-core` (The Contract)
*   **Role:** The foundational crate. Contains the `Ast`, `Model`, `Field` structs, the `la_arena` setup, and the `CodeRenderer` trait. It has zero dependencies on parsing or codegen logic.
*   **AI Task:** Define the data structures based on the SRS. This agent must finish first (or provide a mock interface) so others can begin.

#### 2. `osdl-parser` (Depends on `osdl-core`)
*   **Role:** Takes raw `.osdl` text and converts it to `osdl_core::Ast`.
*   **AI Task:** Write the `.pest` grammar file. Handle indentation rules, type inference, and emit `miette`-compatible errors if parsing fails.

#### 3. `osdl-codegen` (Depends on `osdl-core`)
*   **Role:** The shared code generation utility crate. Contains helper functions for using `syn` and `quote` to build Rust ASTs, format them with `prettyplease`, and write them to the filesystem.
*   **AI Task:** Write the generic file-writer and `syn` tree builder utilities.

#### 4. `osdl-codegen-seaorm` (Depends on `osdl-core`, `osdl-codegen`)
*   **Role:** Implements `CodeRenderer` for SeaORM 2.x.
*   **AI Task:** Write the `quote!` macros to generate SeaORM `DeriveEntityModel` structs, `Relation` enums, and `ActiveModel` implementations. *Can work in parallel with the Mongo agent.*

#### 5. `osdl-codegen-mongo` (Depends on `osdl-core`, `osdl-codegen`)
*   **Role:** Implements `CodeRenderer` for MongoDB 3.x.
*   **AI Task:** Write the `quote!` macros to generate Serde structs with `bson` types, and generate the `$jsonSchema` validator strings. *Can work in parallel with the SeaORM agent.*

#### 6. `osdl-migrator` (Depends on `osdl-core`)
*   **Role:** The diffing engine. Deserializes `osdl.lock` (using `serde_json` + `sha2`), compares it to the current AST, and produces a list of migration actions (CreateTable, AddColumn, etc.).
*   **AI Task:** Write the AST diffing algorithm. It does *not* execute the migrations, it just calculates the delta. *Can work entirely in parallel with the parser and codegen agents.*

#### 7. `osdl-cli` (Depends on all)
*   **Role:** The final binary. Uses `clap` to parse commands (`build`, `migrate`), calls the `osdl-parser`, runs the correct `osdl-codegen-*` renderer, and uses `tokio` to execute database migrations via `osdl-migrator`.
*   **AI Task:** Write the CLI wiring, `tracing` setup, and `miette` error handling. *This agent must wait for all others to finish, or use mock implementations.*

---

### Workspace `Cargo.toml`

To make this work seamlessly, the root `Cargo.toml` defines the workspace and shared dependencies, so agents don't have to manage versions individually.

```toml
[workspace]
members = [
    "crates/osdl-core",
    "crates/osdl-parser",
    "crates/osdl-codegen",
    "crates/osdl-codegen-seaorm",
    "crates/osdl-codegen-mongo",
    "crates/osdl-migrator",
    "crates/osdl-cli",
]
resolver = "2"

[workspace.package]
edition = "2024"
version = "0.1.0"

[workspace.dependencies]
# Shared versions to ensure all crates use the same dependencies
clap = { version = "4.6", features = ["derive"] }
miette = { version = "7.6", features = ["fancy"] }
thiserror = "2.0"
tracing = "0.1.44"
tracing-subscriber = "0.3.19"

pest = "2.8.7"
pest_derive = "2.8.7"
la_arena = "0.3.1"

syn = { version = "3.0", features = ["full", "parsing"] }
quote = "1.0.47"
proc-macro2 = "1.0.92"
prettyplease = "0.2.37"

serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.150"
sha2 = "0.11"

# Internal crates
osdl-core = { path = "crates/osdl-core" }
osdl-parser = { path = "crates/osdl-parser" }
osdl-codegen = { path = "crates/osdl-codegen" }
osdl-codegen-seaorm = { path = "crates/osdl-codegen-seaorm" }
osdl-codegen-mongo = { path = "crates/osdl-codegen-mongo" }
osdl-migrator = { path = "crates/osdl-migrator" }
```

### Why this is perfect for AI Agents:
1. **No Merge Conflicts:** Agent A working on `osdl-parser` will never conflict with Agent B working on `osdl-codegen-seaorm` because they are editing entirely different directories.
2. **Clear Boundaries:** An AI agent can be given a very specific prompt: *"You are working on the `osdl-codegen-seaorm` crate. Your only job is to implement the `CodeRenderer` trait for SeaORM. Here is the trait definition in `osdl-core`."*
3. **Parallel Compilation:** While Agent A is iterating on `osdl-parser`, Agent B can still compile and test `osdl-codegen-seaorm` using mock AST data, without being blocked by parser bugs.

To kick off the parallel AI coding, the very first thing we need to define is the `osdl-core` crate (the AST structures and the `CodeRenderer` trait). Shall I generate the `osdl-core` specification so the agents can start building?
