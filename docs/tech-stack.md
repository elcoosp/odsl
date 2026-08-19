# ODSL Compiler – Tech Stack (Production‑Ready)

## Overview

This stack is chosen to meet three core goals:

- **Blazing performance** – p99 < 500ms for 100 models.
- **Flawless Developer Experience** – beautiful diagnostics (`miette`), structured logging (`tracing`), and deterministic snapshots (`insta`).
- **Zero‑string code generation** – using `syn` + `quote` + `prettyplease` to guarantee syntactically valid Rust output.

All dependencies are up‑to‑date with their latest **stable** releases.

---

## 1. CLI & Terminal UX

| Crate | Version | Purpose |
|-------|---------|---------|
| `clap` | **4.6** | CLI argument parsing with `derive` macros; handles `init`, `build`, `migrate`. |
| `miette` | **7.6** | Rich, coloured diagnostic reports (like Rustc) – points to exact lines and columns. |
| `thiserror` | **2.0** | Error enum definition; **major version update** – adapt to new API. |
| `tracing` | **0.1.44** | Structured, leveled logging; used with `tracing-subscriber` for `-v` verbosity. |
| `tracing-subscriber` | **0.3.19** | Subscriber for `tracing`; enables pretty console output. |

---

## 2. Parsing & Lexing

| Crate | Version | Purpose |
|-------|---------|---------|
| `pest` | **2.8.7** | PEG‑based parser; handles indentation‑sensitive grammar. |
| `pest_derive` | **2.8.7** | Procedural macro to derive `Parser` from a `.pest` grammar file. |

---

## 3. AST & Validation

| Crate | Version | Purpose |
|-------|---------|---------|
| `la_arena` | **0.3.1** | Arena allocation with stable indices (`Idx<T>`); enables cyclic references without fighting the borrow checker. |
| `paste` | **1.0.15** | Macro for string concatenation; useful for generating struct names dynamically. |

---

## 4. Code Generation & Rendering

| Crate | Version | Purpose |
|-------|---------|---------|
| `syn` | **3.0** | Full Rust AST representation; **major version** – ensure you update your code to the new API. |
| `quote` | **1.0.47** | Quasi‑quoting to build Rust code via `quote!` macro. |
| `proc-macro2` | **1.0.92** | Underlying token stream for `syn`/`quote`; works outside procedural macros. |
| `prettyplease` | **0.2.37** | Formats the `syn` AST into beautifully indented Rust code – produces human‑readable output. |

---

## 5. Lockfile & Diffing

| Crate | Version | Purpose |
|-------|---------|---------|
| `serde` | **1.0.229** | Serialization/deserialization of the AST to/from the lockfile. |
| `serde_json` | **1.0.150** | Fast JSON serialisation (chosen for lockfile performance). |
| `sha2` | **0.11** | SHA‑256 hashing for deterministic checksums (REQ‑NFR‑DET‑001). |

---

## 6. Database Driver Integration (for migrations)

| Crate | Version | Purpose |
|-------|---------|---------|
| `sea-orm-migration` | **2.0** | **Officially stable** (released July 2026). Generates and executes SQL migrations for SQLite / Postgres. |
| `mongodb` | **3.8** | Official MongoDB driver; used to apply schema validators and indexes. |
| `tokio` | **1.52.3** | Asynchronous runtime – required for all database operations. |

---

## 7. Testing & Performance

| Crate | Version | Purpose |
|-------|---------|---------|
| `insta` | **1.39** | Snapshot testing – captures generated Rust code and highlights diffs. |
| `proptest` | **1.4** | Property‑based testing – fuzzes the parser with random valid/invalid inputs. |
| `criterion` | **0.5** | Benchmarking – produces HTML reports to enforce the 500ms SLO. |

---

## Full `Cargo.toml` (Updated)

```toml
[package]
name = "odsl"
version = "0.1.0"
edition = "2024"

[dependencies]
# CLI & UX
clap = { version = "4.6", features = ["derive"] }
miette = { version = "7.6", features = ["fancy"] }
thiserror = "2.0"
tracing = "0.1.44"
tracing-subscriber = "0.3.19"

# Parsing & AST
pest = "2.8.7"
pest_derive = "2.8.7"
la_arena = "0.3.1"
paste = "1.0.15"

# Code Generation
syn = { version = "3.0", features = ["full", "parsing"] }
quote = "1.0.47"
proc-macro2 = "1.0.92"
prettyplease = "0.2.37"

# Lockfile & Diffing
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.150"
sha2 = "0.11"

# Database Execution
sea-orm-migration = "2.0"
mongodb = "3.8"
tokio = { version = "1.52.3", features = ["full"] }

[dev-dependencies]
insta = "1.39"
proptest = "1.4"
criterion = "0.5"

[[bench]]
name = "compiler_bench"
harness = false
```

---

## Important Version‑Upgrade Notes

1. **`syn` 3.0** – Contains breaking changes in the AST types. Update your code to match the new API (see [release notes](https://github.com/dtolnay/syn/releases/tag/3.0.0)).
2. **`thiserror` 2.0** – Uses a new attribute syntax; adjust your error definitions accordingly.
3. **`sha2` 0.11** – Minor breaking changes; primarily affects the `digest` trait usage.
4. **SeaORM 2.0** – **Now stable**. The new “dense” entity format places relations directly on the `Model` struct – you can leverage this for cleaner generated code.

---

## Why This Stack Is a Killer

- **Zero‑string codegen** – `syn` + `quote` guarantees that the generated Rust code is always syntactically valid, eliminating macro errors.
- **`la_arena`** – Borrowed from `rust-analyzer`, it makes cyclic ASTs trivial to handle.
- **`miette`** – Produces compiler‑grade error messages that guide developers instantly to the issue.
- **Determinism** – SHA‑256 of the AST ensures that lockfiles and generated files are byte‑for‑byte identical across runs.
- **Snapshot testing** – `insta` lets you review any change to the generated code with a simple `cargo insta review`.
