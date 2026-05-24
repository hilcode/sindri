# CLAUDE.md

Guidance for AI assistants working in this repository.

## What Sindri is

A declarative, lifecycle-driven build tool with incremental correctness, written in
Rust. Build files are written in [Nickel](https://nickel-lang.org/).

Read **`DESIGN.md`** before making changes — it is the source of truth for *what*
the tool does and *why*: the workspace/module model, lifecycle, plugins, task
graph, incremental correctness, BSP, and dependency management.

## Coding conventions

**Follow `CODING_GUIDELINES.md`.** It is binding for all code here — precise names,
no abbreviations, encapsulated fields, newtypes over primitives, direct imports,
explicit `let` types. When in doubt, match the surrounding code.

## Commands

The `justfile` is the entry point (run `just` to list recipes):

- `just test` — full suite (unit + integration). `just test-unit` / `just test-integration` to narrow.
- `just lint` — `cargo clippy -- -D warnings`.
- `just format` — `cargo fmt` (`max_width = 120`).
- `just build [release]` — build (runs `format` first).
- `just coverage` / `just coverage-html` — coverage via `cargo llvm-cov`.

The environment is managed by Nix + Devenv; all tooling (Go, etc.) comes from the
Devenv shell, not the host `PATH`.

## Architecture notes

- **Side effects go through traits.** `FileSystem` → `Runtime` → `Bootstrap` in
  `runtime.rs` abstract the filesystem, clock, command execution, logging, and
  output. Library code never touches `std::fs` / `std::process` directly, so tests
  can stub everything via `DummyRuntime`. Preserve this — it is what keeps the test
  suite hermetic.
- **The bootstrap-before-runtime split is deliberate.** The log destination lives
  inside the workspace's build directory, so the workspace must be located (with a
  bare `FileSystem`) before the full `Runtime` can be built. Don't collapse the two.
- **Typed paths everywhere.** `PathBuf` is converted to a typed path at the edge;
  everything downstream is `Absolute*` / `Relative*` / `WorkspaceRoot` / `BuildFile`
  / `ConfigFile`. Keep raw paths out of the interior.
