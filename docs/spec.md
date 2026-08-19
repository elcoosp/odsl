# ODSL (Open Schema Definition Language) - Master Specification

| Field | Value |
|-------|-------|
| **Project** | ODSL (Open Schema Definition Language) |
| **Version** | 1.0 (Draft) |
| **Date** | Current Date |
| **Author** | User (assisted by AI) |
| **Status** | Draft — Pending Review |
| **Target Stack** | Rust Edition 2024, SeaORM 2.x, MongoDB 3.x |

---

## Table of Contents
1. [Level 0: Vision & Strategic Alignment](#level-0-vision--strategic-alignment)
2. [Level 1: Business & Stakeholder Requirements (BRS)](#level-1-business--stakeholder-requirements-brs)
3. [Level 2: Software Requirements Specification (SRS)](#level-2-software-requirements-specification-srs)
4. [Level 3: Architecture & Design Specification](#level-3-architecture--design-specification)
5. [Level 4: Behavioral Spec & Test Verification](#level-4-behavioral-spec--test-verification)

---

## Level 0: Vision & Strategic Alignment

### 1. Vision Statement
To make database schema definition and ORM boilerplate generation so token-efficient and paradigm-agnostic that developers and AI agents can design and port complex data layers in seconds, without writing a single line of SQL or Rust macros.

### 2. Elevator Pitch
"For Rust developers and AI agents who need to design database schemas quickly, ODSL is a token-optimized schema definition language and compiler that generates Rust SeaORM and MongoDB boilerplate automatically. Unlike writing YAML/JSON or raw SQL migrations, ODSL uses a purely indentation-based, DRY syntax that cuts LLM token usage by 80% and seamlessly transpiles to both SQL and NoSQL paradigms without database-specific escape hatches."

### 3. Problem Statement & Business Context
Developers and AI agents currently face three major bottlenecks when defining database schemas:
1. **LLM Token Waste:** Generating verbose JSON/YAML schemas or raw SQL DDL consumes excessive context window space and inflates AI generation costs.
2. **ORM Boilerplate:** Writing Rust SeaORM entities, relations, and migration files by hand is tedious, repetitive, and prone to macro syntax errors.
3. **Paradigm Lock-in:** Switching databases (e.g., from SQLite to Postgres, or SQL to MongoDB) requires massive rewrites of schema definitions and model code, usually requiring database-specific escape hatches that break abstraction.

### 4. Target Users & Anti-Scope
**Target Users:** Solo developers and small startup teams using Rust, alongside AI agents/LLMs acting as autonomous co-developers.

**Explicit Anti-Scope (Not Targeted):**
- Large enterprise teams requiring GUI-driven data modeling tools (e.g., ErWin).
- Developers using dynamically typed languages (Python, JS) where ORMs lack strict typing.
- Non-technical users (the DSL requires developer-level knowledge of data types and relationships).

### 5. Value Proposition & Differentiators
1. **Ultra-Compact DSL:** A purely indentation-based, punctuation-free syntax optimized specifically for LLM token economy, reducing token usage by up to 80%.
2. **Auto-Diffing Migrations:** Developers/LLMs only edit the abstract schema; the compiler automatically diffs against a lockfile and generates the `ALTER TABLE` or Mongo validator scripts.
3. **Intent-Based Mapping:** Declarative flags (e.g., `-fulltext`, `-tz`) seamlessly transpile to database-specific implementations (PG `GIN`, SQLite `FTS5`, Mongo Text Indexes) without raw SQL escape hatches.

### 6. Desired Outcomes & Success Metrics
- **Outcome 1: Maximize LLM Token Economy** — LLMs generate ODSL schemas using at least 70% fewer tokens than equivalent JSON/YAML + SQL representations.
- **Outcome 2: Accelerate Developer Velocity** — A developer/LLM can define a 10-table relational schema and generate Rust SeaORM entities + migrations in under 60 seconds.
- **Outcome 3: Ensure Paradigm Portability** — An ODSL file can be transpiled to both Postgres (SeaORM) and MongoDB (Serde + Driver) with zero manual changes to the source `.odsl` file.

### 7. Goals and Non-Goals
**Goals**
- Provide a purely indentation-based, DRY syntax for schema definition.
- Automatically generate Rust SeaORM 2.x entities (Postgres/SQLite) and MongoDB Serde structs.
- Automatically diff `.odsl` files against a lockfile to generate migrations.
- Map high-level intent flags to database-specific implementations.

**Non-Goals (Anti-Scope)**
- No Visual GUI: ODSL will remain strictly a CLI and text-based tool.
- No Non-Rust Targets: v1 will not generate code for Python, TypeScript, Go, or other language ORMs.
- No Reverse Engineering: The compiler will not parse existing SQL databases to generate ODSL files.

### 8. Strategic Constraints
- **Single Binary Distribution:** Must be distributable as a standalone Rust binary via `cargo install odsl`.
- **Zero Runtime Overhead:** ODSL is strictly a build-time/compile-time tool; no ODSL code runs in the final application binary.

### 9. Operational Concept & High-Level Scenarios
**Concept of Operations:** ODSL operates as a standalone CLI tool. The developer (or AI agent) defines the data model in a `.odsl` file. Running `odsl build` parses this file and generates the Rust SeaORM entities or MongoDB Serde structs directly into the `src/entity` directory. Running `odsl migrate up` diffs the current `.odsl` file against an `odsl.lock` file and automatically generates/runs the necessary SQL migrations or Mongo schema validators.

**Scenario: Adding a new feature (e.g., User Comments)**
1. Prompt: Dev asks LLM to "Add a Comment model linked to the User and Post models."
2. LLM Output: LLM outputs ~5 lines of ODSL syntax.
3. Execution: Dev runs `odsl build` and `odsl migrate up`.
4. Result: Feature implemented in seconds with zero macro syntax errors.

### 10. Stakeholders & Governance
- **Sponsor/Owner:** Solo maintainer.
- **Contributors:** Open-source community via Pull Requests.
- **Primary "Users":** Human Rust developers and AI Agents.

### 11. Risks & Mitigations
- **Risk 1: Compiler Complexity.** Building a robust AST parser and auto-diffing engine in Rust. *Mitigation:* Use proven tools (`pest`, Arena allocation).
- **Risk 2: Database Subtleties.** Mapping abstract intents to SeaORM macros and Mongo validators. *Mitigation:* Exhaustive BDD testing and snapshot validation.
- **Risk 3: LLM Adoption.** Getting LLMs to reliably output ODSL syntax. *Mitigation:* Design the DSL to be so simple and token-efficient that LLMs naturally prefer it.

---

## Level 1: Business & Stakeholder Requirements (BRS)

### 1. Glossary & Ubiquitous Language
- **Model:** A top-level data entity (maps to a Table in SQL or a Collection in NoSQL).
- **Field:** A defined attribute within a Model (maps to a Column in SQL or a Document key in NoSQL).
- **Intent Flag:** A modifier attached to a Field or Model (e.g., `-fulltext`, `-tz`, `-uniq`) that declares *desired behavior* rather than database-specific implementation.
- **Lockfile (`odsl.lock`):** A generated snapshot of the last compiled schema state, used to calculate diffs for auto-migrations.
- **Transpilation:** The act of converting an `.odsl` file into target-specific Rust code and database migrations.

### 2. Conceptual Domain Model
The core entities of the ODSL tool itself are:
1. **Schema File (.odsl):** The source of truth, written by humans/LLMs.
2. **AST (Abstract Syntax Tree):** The in-memory representation of the parsed schema.
3. **Target Renderer:** A module (e.g., `SeaORMRenderer`, `MongoRenderer`) that converts the AST into Rust code.
4. **Migration Engine:** The module that diffs the current AST against the Lockfile AST to produce migration scripts.

### 3. Business Rules & Policies
- **BR-001:** Every `Model` must define exactly one Primary Key (`-pk`) or NoSQL Partition Key (`-partition`).
- **BR-002:** References (e.g., `author User.id`) must always point to a valid, existing `Model` within the parsed ODSL context. Unresolved references result in a compilation failure.
- **BR-003:** The compiler must fail (not warn) if a target database cannot support a specific intent flag (e.g., applying `-partition` to a relational SQLite target without an index).

### 4. Stakeholders & User Classes
- **Primary: Human Rust Developer**
  - *JTBD:* Design database schemas rapidly and generate boilerplate-free Rust code without memorizing ORM macros.
- **Secondary: AI Agent (LLM)**
  - *JTBD:* Output database schemas using minimal tokens, maximizing context window efficiency while avoiding syntax errors.
- **Tertiary: Open-Source Contributor**
  - *JTBD:* Extend the compiler to support new target databases or ORMs via modular renderers.

### 5. System-in-Context Processes & Operational Concept
**Core Workflow (MVP - Linear Flow):**
1. Parse `.odsl` file(s).
2. Build AST, resolving references and checking Business Rules.
3. Render Rust code (SeaORM entities or MongoDB Serde structs).
4. Diff current AST against `odsl.lock`.
5. Generate and execute database migration scripts.

### 6. Quality Expectations (NFRs)
- **Performance:** Parsing and generating code for a 50-model schema should take less than 1 second.
- **Determinism:** Running the compiler twice on the same `.odsl` file must produce byte-for-byte identical Rust code and migration files.
- **Offline Operation:** The compiler must never require network access; all transpilation and diffing happens locally.

### 7. Constraints
- Generated SeaORM entities must not rely on custom runtime crates; they must only use `sea_orm` and standard `serde`.
- Generated MongoDB structs must use the official `mongodb` and `bson` crates, avoiding third-party ODMs.

---

## Level 2: Software Requirements Specification (SRS)

### 1. Functional Capabilities & Behavior
**REQ-FUNC-001 (Parsing & Lexing):** The system shall parse `.odsl` files using a strict Parsing Expression Grammar (PEG) to enforce an indentation-based hierarchy without relying on curly braces or semicolons. (Priority: Must)

**REQ-FUNC-002 (Type Inference):** When a field type is omitted, the system shall infer the primitive type based on standard naming conventions (e.g., `created_at` -> `datetime`, `email` -> `string`, fields ending in `_id` matching another model -> reference/foreign key). (Priority: Should)

**REQ-FUNC-003 (Code Generation Structure):** The system shall generate modular Rust files (one file per model, e.g., `src/entity/user.rs`) alongside a `mod.rs` file that exports the generated modules. (Priority: Must)

**REQ-FUNC-004 (Reference Resolution):** When resolving references, the system shall fail compilation with a precise error message if the target `Model` does not exist within the parsed AST context. (Priority: Must)

**REQ-FUNC-005 (Auto-Diffing Migrations):** When the `migrate` command is executed, the system shall diff the current AST against the `odsl.lock` file and automatically generate the necessary `ALTER TABLE` (SQL) or `$jsonSchema` updates (MongoDB). (Priority: Must)

### 2. Edge Cases & Unwanted Behavior (EARS Syntax)
**REQ-FUNC-006 (Cyclic Dependencies):** *If* a cyclic reference is detected between models, *then* the compiler shall abort compilation and output a "Cyclic Dependency Detected" error message.

**REQ-FUNC-007 (Invalid Intent Mapping):** *If* an intent flag is applied to an incompatible field type (e.g., `-fulltext` applied to an `int`), *then* the compiler shall reject the schema during AST validation and output a "Type Mismatch" error.

**REQ-FUNC-008 (Target Incompatibility):** *If* an intent flag is used but the target database does not support it natively, *then* the compiler shall fail fast, abort code generation, and log a descriptive error explaining the incompatibility.

### 3. External CLI Interfaces & Commands
The ODSL CLI must provide the following command interface:
- `odsl init`: Scaffolds a new ODSL project by creating an empty `schema.odsl` file and an `odsl.lock` file.
- `odsl build`: Parses the `.odsl` file and generates Rust entity files.
  - *Flag:* `--target [seaorm|mongo]`
- `odsl migrate create`: Diffs the current `.odsl` file against `odsl.lock` and generates migration scripts.
- `odsl migrate up`: Executes pending migration scripts against the active database.

### 4. Quality Requirements (NFRs)
- **REQ-NFR-PERF-001 (Performance):** Parsing and generating code for 100 models must complete in p99 < 500ms.
- **REQ-NFR-DET-001 (Determinism):** The compiler must use stable SHA256 hashing of the AST to generate the `odsl.lock` file, ensuring consistent diffing and byte-for-byte identical outputs.
- **REQ-NFR-OFFL-001 (Offline Operation):** The compiler must function entirely offline.

### 5. Constraints
- **Generated Code Constraints:** Generated code must strictly target the `sea-orm 2.x` and `mongodb 3.x` driver APIs, utilizing only standard `serde`.
- **Compiler Constraint:** The ODSL compiler itself must be written in Rust Edition 2024.

---

## Level 3: Architecture & Design Specification

### 1. System Overview (C4 Model - Component Level)
The ODSL compiler is structured as a single binary CLI tool with four distinct internal components:
1. **CLI Interface (`clap`):** Handles `init`, `build`, and `migrate` commands.
2. **Parser & Lexer (`pest`):** Reads `.odsl` files and constructs an in-memory AST.
3. **AST & Validator (Arena):** Resolves cross-references, infers types, and enforces Business Rules.
4. **Rendering Engine (`trait CodeRenderer`):** Consumes the validated AST to generate Rust code and diffs against `odsl.lock` to generate migrations.

### 2. Architecture Decision Records (ADRs)

**ADR-001: Use `pest` (PEG) for Parsing**
- *Context:* We need to parse an indentation-based DSL without curly braces.
- *Decision:* Use the `pest` crate.
- *Consequences:* + Declarative grammar is easy to read/modify. - Requires careful management of whitespace rules in the grammar file.

**ADR-002: Use Arena Allocation for the AST**
- *Context:* Models contain cyclic references (e.g., User has Posts, Post has Author).
- *Decision:* Use an Arena tree structure where nodes reference each other by stable IDs rather than Rust references.
- *Consequences:* + Bypasses strict borrow checker rules for cyclic graphs. + Makes AST traversal and validation much safer to write.

**ADR-003: Trait-Based Renderers**
- *Context:* We need to support multiple targets (SeaORM, MongoDB) without tangling logic.
- *Decision:* Define a `trait CodeRenderer` that accepts the validated AST.
- *Consequences:* + Contributors can add new targets by simply implementing the trait. + Keeps the core compiler logic decoupled from specific ORM APIs.

### 3. Data Flow & Rendering Engine
1. **Input:** `.odsl` file is read as a UTF-8 string.
2. **Parse:** `pest` matches the string against the ODSL grammar, producing a `Pair` stream.
3. **AST Build:** The stream is consumed, and an Arena-allocated tree is built.
4. **Validate:** The compiler walks the tree to resolve `Entity.id` references and validate flags.
5. **Render:** The `CodeRenderer` implementation converts AST nodes into Rust code strings (using `quote!` or `syn` if needed, or direct string formatting).
6. **Diff:** The current AST is compared to the deserialized `odsl.lock` AST.
7. **Output:** Files are written to `src/entity/` and `migrations/`.

---

## Level 4: Behavioral Spec & Test Verification

### 1. BDD Acceptance Criteria (Gherkin Scenarios)

**Scenario: Compiler fails on cyclic dependencies**
```gherkin
Given a schema file with Model A referencing Model B
And Model B references Model A
When the parser builds the AST
Then the validator should detect a cycle
And the compiler should abort with error "Cyclic Dependency Detected between A and B"
```

**Scenario: Compiler rejects incompatible flags**
```gherkin
Given a schema with a field "age int -fulltext"
When the validator checks the intent flags
Then the compiler should abort with error "Type Mismatch: -fulltext cannot be applied to int"
```

**Scenario: Compiler fails when target DB lacks feature**
```gherkin
Given a schema with "id uuid -partition"
When the build command runs with "--target seaorm-sqlite"
Then the compiler should abort with error "Target Incompatibility: SQLite does not support -partition natively"
```

**Scenario: Adding a new field generates an ALTER TABLE**
```gherkin
Given an odsl.lock file representing a "User" model with fields [id, email]
And the current .odsl file has "User" with fields [id, email, name]
When the migrate create command is executed
Then a new migration file should be generated
And the migration should contain "ALTER TABLE users ADD COLUMN name TEXT"
```

### 2. Test Strategy & Implementation

**1. Unit & Snapshot Testing (The Core Engine)**
- *Requirement:* Every `odsl build` command must produce byte-for-byte identical output.
- *Implementation:* Use the `insta` crate for snapshot testing. We will create a `tests/snapshots/` directory containing expected outputs for various `.odsl` inputs (e.g., `simple_model.odsl`, `complex_relations.odsl`, `mongo_target.odsl`). If generated code drifts from the snapshot, CI fails.

**2. Property-Based Testing (The Parser)**
- *Requirement:* The `pest` parser must never panic on malformed input.
- *Implementation:* Use `proptest` to generate thousands of random strings (valid and invalid ODSL syntax) and feed them to the parser. The test passes as long as the parser gracefully returns an error and never triggers a Rust `panic!`.

**3. Performance Testing (NFR Verification)**
- *Requirement:* Verify p99 < 500ms for 100 models (REQ-NFR-PERF-001).
- *Implementation:* Use the `criterion` crate to benchmark the "Parse -> Validate -> Render" pipeline on a synthetic 100-model `.odsl` file. Add this to CI to catch performance regressions.

### 3. Requirements Traceability Matrix (RTM) (Excerpt)

| Req ID | BRS Rule | SRS Requirement | Architecture | Test Method |
|--------|----------|-----------------|--------------|-------------|
| R-01 | BR-001 | REQ-FUNC-001 | ADR-001 (`pest`) | Unit/Property Test |
| R-02 | BR-002 | REQ-FUNC-004 | ADR-002 (Arena) | BDD Scenario: Cyclic Deps |
| R-03 | BR-003 | REQ-FUNC-008 | ADR-003 (Traits) | BDD Scenario: Target Incompatibility |
| R-04 | NFR-PERF | REQ-NFR-PERF-001 | C4 (Parser) | `criterion` Benchmark |
| R-05 | NFR-DET | REQ-NFR-DET-001 | C4 (Lockfile) | Snapshot Test (`insta`) |

---

*End of Specification Document.*
