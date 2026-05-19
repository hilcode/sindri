# MVP Implementation Plan

## Phase 1 — Nickel integration + workspace/module loading

Goal: `sindri` can find, parse, and validate `sindri.workspace` and `sindri.build` files.
All errors are reported through `miette` with helpful messages and a clear call to action.

Tests in this phase use temporary directories (`tempfile` crate, added as a dev-dependency).

- [x] Add dependencies to `Cargo.toml`: `nickel-lang`, `miette` (with `fancy` feature), `thiserror`, `serde` (with `derive` feature)
- [x] Add `tracing`, `tracing-subscriber`, and `tracing-appender` dependencies; add a `--log` flag to the CLI; when present, initialise a non-blocking file subscriber writing to `<build_dir>/sindri.log` immediately after the workspace is loaded
  - [x] Test: running with `--log` creates `<build_dir>/sindri.log` containing trace output
  - [x] Test: running without `--log` creates no log file
  - [x] Test: terminal output contains no tracing output regardless of `--log`
- [x] Verify `nickel-lang` compiles and its API matches what was researched (evaluate a trivial `.ncl` expression, extract a value)
  - [x] Test: evaluating a Nickel record and extracting a string field succeeds

- [x] Define Rust structs for `Workspace` and `Module` (with `serde::Deserialize`); use newtypes throughout (`Version`, `ModuleName`, `WorkspaceName`, `BuildDirectory`, `Repository`, `Language`, `WorkspaceRoot`, `BuildFile`)
  - [x] Test: `Workspace` deserializes correctly from a valid Nickel record
  - [x] Test: `Module` deserializes correctly from a valid Nickel record
  - [x] Test: missing required field in `Workspace` produces a `Schema` error
  - [x] Test: missing required field in `Module` produces a `Schema` error
  - [x] Test: `build_dir` defaults to `.target` when omitted from `sindri.workspace`

- [x] Define all error types using `thiserror` + `#[diagnostic]`; every error must include a `help` message with a concrete call to action
  - [x] Test: every error variant has a non-empty `help` string
  - [x] Test: every error variant has a diagnostic `code`

- [x] Wrap Nickel evaluation errors so they are presented through `miette` with source context
  - [x] Test: a Nickel syntax error in `sindri.workspace` produces a `NickelEval` error containing the file path

- [x] Implement workspace root detection: walk up from CWD until `sindri.workspace` is found; error if none found before the filesystem root; error if `sindri.workspace` is a symlink
  - [x] Test: finds the workspace root when invoked from the workspace root directory
  - [x] Test: finds the workspace root when invoked from a subdirectory
  - [x] Test: returns `WorkspaceNotFound` when no `sindri.workspace` exists anywhere in the tree
  - [x] Test: returns `SymlinkNotSupported` when `sindri.workspace` is a symlink

- [x] Load `sindri.workspace`: evaluate via Nickel, deserialize into `Workspace` struct
  - [x] Test: loads a valid `sindri.workspace` and returns the expected field values
  - [x] Test: returns `NickelEval` on a file with a Nickel syntax error
  - [x] Test: returns `Schema` when a required field is missing

- [x] Write the `Workspace` Nickel contract (`.ncl` file shipped with Sindri) and apply it during loading for richer error messages
  - [x] Test: contract violation (e.g. wrong type for a field) produces an error with source location context
  - [x] Test: valid `sindri.workspace` passes the contract without error

- [x] Write the `Module` Nickel contract (`.ncl` file shipped with Sindri) and apply it during loading
  - [x] Test: contract violation produces an error with source location context
  - [x] Test: valid `sindri.build` passes the contract without error

- [x] Implement module entry-point detection: walk up from CWD until `sindri.build` (or `sindri-<qualifier>.build`) is found, stopping at the workspace root; error if none found; error if the build file is a symlink
  - [x] Test: finds `sindri.build` in the current directory
  - [x] Test: finds `sindri.build` in a parent directory (but not past the workspace root)
  - [x] Test: finds `sindri-kotlin.build` when no `sindri.build` exists
  - [x] Test: does not escape past the workspace root into parent directories
  - [x] Test: returns `ModuleNotFound` when no build file exists within the workspace
  - [x] Test: returns `SymlinkNotSupported` when the build file is a symlink

- [x] Load `sindri.build`: evaluate via Nickel, deserialize into `Module` struct
  - [x] Test: loads a valid `sindri.build` and returns the expected field values
  - [x] Test: returns `NickelEval` on a file with a Nickel syntax error
  - [x] Test: returns `Schema` when a required field is missing

---

## Phase 2 — Built-in Go plugin + task graph

Goal: Sindri knows what tasks exist and in what order they run.
`sindri lifecycle` prints the resolved lifecycle.

- [x] Add `tempfile` as a dev-dependency
- [x] Define Rust types: `Plugin`, `Task`, `Step` (matching the shape a loaded plugin would produce)
  - [x] Test: the fixed lifecycle sequence contains the expected steps in the correct order
- [x] Implement the fixed lifecycle sequence as a static ordered list
- [x] Implement the built-in Go plugin as a static Rust value (same `Plugin` type as above, not loaded from disk)
  - [x] Test: the Go plugin contributes tasks to the correct lifecycle steps
- [x] Implement task graph construction for a single module: collect tasks bound to each step, add step-ordering edges
  - [x] Test: tasks within the same step have no ordering edges between them
  - [x] Test: all tasks in step N precede all tasks in step N+1
- [x] Add `lifecycle` subcommand to the CLI: print the resolved lifecycle steps and the tasks bound to each
  - [x] Integration test: `sindri lifecycle` output contains the expected steps and Go tasks
- [x] Add `compile` subcommand to the CLI (no execution yet — just resolves and prints the task graph that would run)
  - [x] Integration test: `sindri compile` prints the expected task graph

---

## Phase 3 — Task execution

Goal: `sindri compile` actually runs `go build` and reports success or failure.

- [x] Implement task executor: run a shell command, stream stdout/stderr to the terminal
  - [x] Test: a succeeding command exits 0 and its stdout is captured
  - [x] Test: a failing command exits non-zero and its stderr is captured
- [x] Handle task failure: report via `miette` with the failing command and its output, exit non-zero
  - [x] Test: task failure produces a `miette` error with the command and output in the message
- [x] Wire the `compile` subcommand to execute the task graph up to and including `compile`
- [x] Implement parallelism: tasks within the same lifecycle step with no dependency edges run concurrently
  - [x] Test: two independent tasks in the same step complete in less time than their sequential sum

### Progress output

- [x] Print a start line when each task begins: `  → <task-name>` (or similar)
- [x] Print a completion line when each task finishes, including elapsed time: `  ✓ <task-name> (0.4s)` on success, `  ✗ <task-name> (0.4s)` on failure
- [x] Add `--quiet` / `-q` flag: suppress all progress output; only errors are shown
  - [x] Test: `--quiet` produces no progress lines on a successful build
  - [x] Test: `--quiet` still shows errors on a failed build
- [x] Task stdout/stderr is shown only on failure by default; shown always under `--verbose` / `-v`
  - [x] Test: a passing task produces no stdout/stderr output in the terminal by default
  - [x] Test: a passing task's output is shown with `--verbose`
  - [x] Test: a failing task's output is always shown regardless of verbosity
- [x] Progress output and tracing output never interleave on the terminal

### Telemetry

Format and rationale: DESIGN.md §7 (Telemetry).

- [x] Add `serde_json` dependency
- [x] Define a `TraceEvent` struct serialising to the Chrome trace `X` event shape (`name`, `ph`, `ts`, `dur`, `pid`, `tid`, `args`)
- [x] Record one event per executed task: `ts` = µs from process start, `dur` = wall-clock duration in µs, `tid` = executor thread index
- [x] Write `<build_dir>/telemetry.json` at build end (success or failure), wrapped in `{"traceEvents": [...]}`
  - [x] Test: a build produces `telemetry.json` containing one event per executed task
  - [x] Test: all event `dur` values are positive
  - [x] Test: parallel tasks on different threads have distinct `tid` values

- [x] Integration test: `sindri compile` in a valid Go module runs `go build` and exits 0

---

## Phase 4 — Incremental correctness

Goal: A second `sindri compile` on an unchanged tree skips all tasks immediately.

### Structured commands

A task's command becomes a **list of strings** (program + arguments), run directly with no shell
(resolves DESIGN §15 OQ#2). This replaces the executor's current whitespace-split of a single command
string, which breaks on quoting and paths with spaces. The "figure out the exact shape" happens here,
as we wire up the `go` tool invocations.

- [x] Change the command type from a shell string to an argument list (`[program, arg, …]`); update the Go plugin's tasks and the executor to spawn it directly, no shell
  - [x] Test: an argument containing a space is passed as one argument, not split
- [x] Naming: rename `ShellCommand` → `Command` (a program-plus-arguments list); alias `std::process::Command as ProcessCommand` in `runtime.rs`; rename the CLI subcommand enum `Command` → `Action` to free the name

### Glob traversal

Discovery is glob-based. Sindri does not run the native tool's own query (e.g. `go list`); it tracks a
deliberate **platform-independent superset** of each module's source — every file type the tool might
compile — and lets the tool select among them. Over-inclusion only ever costs a spurious rebuild; it
never misses an input. For Go that superset is `**/*.{go,c,h,cc,cpp,cxx,hh,hpp,hxx,m,s,S}` plus
`go.mod`/`go.sum`. (Per-target precision and `//go:embed` via `go list` are future work — DESIGN §16.)

- [x] Add `globset` and `ignore` dependencies (BurntSushi / ripgrep author)
- [x] Configure `WalkBuilder` with `standard_filters(false)` to disable `.gitignore`, hidden-file filtering, and global ignore files — Sindri controls traversal explicitly
- [x] Configure `WalkBuilder` with `follow_links(false)` (the default) — symlinks are not supported in the MVP
- [x] Implement glob expansion: extract the literal prefix before the first wildcard and start `WalkBuilder` traversal there, so unrelated trees are never entered
  - [x] Test: a glob of `src/**/*.go` matches `.go` files under `src/`
  - [x] Test: a glob of `src/**/*.go` does not match files outside `src/`
  - [x] Test: a glob of `src/**/*.go` does not traverse directories outside `src/`
- [x] Design the glob input type to support both include and exclude patterns from the start (even if only includes are used in the MVP)
  - [x] Test: an exclude pattern prevents a matching file from being returned
- [x] Set the Go plugin's input globs to the superset (`**/*.{go,c,h,cc,cpp,cxx,hh,hpp,hxx,m,s,S}` + `go.mod`/`go.sum`)
  - [x] Test: a `.c`/`.h` file alongside a cgo package is tracked as an input

### Hashing

- [x] Add `blake3` dependency
- [x] Implement file hashing with cross-platform determinism guarantees:
  - Normalize CRLF → LF before hashing so a file checked out on Windows produces the same hash as on Linux
  - Normalize all paths to `/`-separated strings relative to the workspace root before hashing — never feed OS-native paths into the hash function
  - Sort file lists lexicographically before hashing — filesystem readdir order is not guaranteed
  - Hash raw file bytes only (after line-ending normalization) — no timestamps, permissions, executable bit, or other OS metadata
  - [x] Test: a file with CRLF line endings produces the same hash as the same file with LF line endings
  - [x] Test: the same set of files produces the same hash regardless of the order they are provided in
  - [x] Test: changing any byte in a file changes its hash
- [x] Include the task declaration itself (command, globs) in the hash so plugin changes force a re-run
  - [x] Test: changing the task command changes the combined hash

### State persistence

The build directory is laid out as `.target/<qualifier>/<step>/<task>/`, where `<qualifier>` is `default` for `sindri.build` and the qualifier name (e.g. `kotlin`) for `sindri-<qualifier>.build`. Each task owns its own directory, so parallel tasks within the same step never contend on the same file.

```
.target/
  default/            # module from sindri.build
    compile/
      go-compile/     # one directory per task
        state.bin     # input + output hashes for this task
  kotlin/             # module from sindri-kotlin.build
    compile/
      kotlin-compile/
        state.bin
```

- [x] Add `rmp-serde` dependency (MessagePack — binary, compact, serde-compatible, platform-independent)
- [x] Define the state data structures: per-task record of input file paths + hashes, output file paths + hashes, and task declaration hash
- [x] Implement the `.target/<qualifier>/<step>/<task>/` directory layout
- [x] Load a task's persisted state from its own directory on startup (missing state = task is dirty)
  - [x] Test: a task with no state file is considered dirty
- [x] Dirtiness check: compare current input and output hashes against persisted state; skip clean tasks
  - [x] Test: a task whose inputs and outputs are unchanged is considered clean
  - [x] Test: a task with a changed input file is considered dirty
  - [x] Test: a task with a missing output file is considered dirty
  - [x] Test: a task with a changed output file is considered dirty
  - [x] Test: a task whose declaration has changed is considered dirty
- [x] Persist state to a task's own directory after successful completion
  - [x] Test: after a successful run, the task is considered clean on the next check

### Telemetry

- [x] Annotate each telemetry event's `args` with `{"cache": "hit"}` or `{"cache": "miss"}` based on the dirtiness check result
  - [x] Test: a cache-hit task has `args.cache == "hit"` in `telemetry.json`
  - [x] Test: a cache-miss task has `args.cache == "miss"` in `telemetry.json`

- [x] Integration test: second `sindri compile` on an unchanged tree prints nothing to do and exits immediately

---

## Phase 5 — Module dependencies

Goal: a module can depend on another module in the same workspace, and Sindri builds the
dependency graph in the correct order with correct incrementality. The explicit
`{ module = "//…" }` declarations in `sindri.build` are the single source of truth for the build
graph (DESIGN §3, §4, §6).

This phase covers *module* (intra-workspace) dependencies only. Resolution of external `{ artifact = … }`
dependencies (the `sindri.lock` lock file, transitive version resolution, fetching — DESIGN §8) is a
separate, later phase; here the `{ artifact = … }` shape is only parsed, not resolved.

The phase is split into subphases so each lands as a small, self-contained step. **Every subphase
must wire what it builds into the real `sindri compile` path** — no subphase adds *substantive
capability* (a type, or the behaviour of a function) that is justified only by its own unit tests.
This is about not building a feature ahead of the code that uses it; it is not a ban on small
test-support helpers. A trivial accessor or constructor added so a test can observe a result, or read
more simply, is fine even if production does not yet call it — judge it by whether it carries real,
otherwise-unused logic, not by whether a non-test caller happens to exist today. Independent modules
run **sequentially in dependency-first order** for now; the design keeps genuine inter-module
parallelism open as a later refinement but must not preclude it.

**Grow the `examples/` directory alongside the code.** Phase 5 is the first time a workspace can hold
more than one module, so as multi-module support lands, add runnable examples that demonstrate it and
keep `examples/README.md` and the examples `justfile` current. An example is only added once the
feature it shows actually builds end-to-end via the installed `sindri` binary.

### The parallelism seam (conceptual)

Independent modules run sequentially for now, but the design must not preclude running them in
parallel later. The intended shape:

- **A module scheduler over the existing per-module engine.** `execute_graph` stays the per-module
  engine (group by step → serial dirtiness pre-pass → parallel `run_misses` → report), unchanged.
  Above it sits a module scheduler that walks the module graph. Today it is a width-1 traversal
  (`for module in topological_order`); the parallel version replaces that loop with a ready-queue —
  a module becomes *ready* once all its dependency modules have finished, and ready modules run
  concurrently on N workers.
- **Topological readiness is the parallelism boundary, and we need it for correctness anyway.** The
  dependency-rebuilt signal (5e) means a module must know whether each dependency had a miss *before*
  it plans, so a module can only start once its dependencies have finished. Sequential topo-order is
  simply the width-1 traversal of that same structure; going parallel swaps the traversal, not the
  data model.

To keep that swap local, subphases 5c–5e must honour four invariants:

1. **`execute_graph` is a pure function of `(module, dependency-status, runtime) → (outcomes, rebuilt)`.**
   It takes an explicit `force_miss` / dependency-status input and returns whether the module rebuilt;
   no cross-module sequencing or shared single-threaded counters are baked in. (`Runtime` is already
   `Sync` with `&self` methods.)
2. **Module-scoped state paths.** `TaskPaths` must key on the module identity (`ModulePath`), not just
   the qualifier — two plain `sindri.build` modules would otherwise both land under
   `.target/default/…` and collide. This is a correctness requirement even sequentially; once each
   module owns a disjoint subtree, concurrent modules never contend.
3. **Global fiber / `tid` allocation.** Fiber ids must come from a global allocator rather than a
   per-step-group index, so parallel modules appear as distinct lanes in the telemetry trace. The
   allocator can come later; new code just must not assume tid uniqueness from a local index.
4. **Funnel all user-facing output through one reporter.** Keep every progress write inside the
   reporter path rather than scattering `writeln!(runtime.output(), …)` through the multi-module code,
   so the parallel scheduler can later swap in a reporter that buffers each module's lines and flushes
   them atomically.

Invariants 3 and 4 are what DESIGN §11's "engine event sink" ultimately addresses (execution emits
`TaskStarted` / `TaskFinished{hit|miss}` / `BuildFinished` events that observers consume); we do not
build that here, only keep output and fiber assignment funnelled so that refactor stays local.

### Phase 5a — Dependency declarations

Goal: `sindri.build` files can declare `dependencies`, and every `sindri compile` parses and
validates them.

- [x] Extend the `Module` struct and the `Module` Nickel contract with a `dependencies` record: `compile` / `export` / `test` / `runtime` / `test-runtime` scopes, each an array of `{ artifact = … }` or `{ module = … }` entries. Re-export is modelled as its own `export` scope (compile-time inputs that are also visible to consumers) rather than a per-entry flag, so `export` cannot be attached to a non-compile dependency at all (DESIGN §4, §8)
  - [x] Test: a `sindri.build` with module and artifact dependencies deserializes into the expected scopes
  - [x] Test: a module with no `dependencies` field loads with empty scopes
  - [x] Test: a per-entry `export` field is a contract error (the closed dependency contract has no such field)
- [x] Define a `ModuleIdentity` newtype for workspace-relative module identities (`//libs/common`, `//libs/common [kotlin]`) that resolves to a `sindri.build` / `sindri-<qualifier>.build` file (DESIGN §4)
  - [x] Test: `//libs/common` resolves to `libs/common/sindri.build`
  - [x] Test: `//tools/codegen [bin]` resolves to `tools/codegen/sindri-bin.build`
- [x] **Wire it in:** validation runs during `Module::load` (already on the `sindri compile` path), so a malformed `dependencies` block fails a real compile with a precise error; the loaded entry module exposes its parsed dependencies to later subphases.

### Phase 5b — Transitive module graph

Goal: `sindri compile` loads the whole reachable module graph from the entry module, surfacing cycle
and non-library errors on the real compile path.

- [x] Load the dependency graph demand-driven from the entry module: follow `{ module = … }` declarations transitively, loading only reachable modules; never walk the filesystem (DESIGN §3)
  - [x] Test: depending on `//libs/common` loads that module
  - [x] Test: a transitive chain (A → B → C) loads all three
  - [x] Test: a module unreachable from the entry point is never loaded
- [x] Reject dependency cycles with a named error listing the modules in the cycle
  - [x] Test: A → B → A produces a cycle error naming the modules involved
- [x] Reject a `dependencies` entry that names a non-`library` module (only libraries may be dependencies — DESIGN §4)
  - [x] Test: depending on an `executable` module is an error
- [x] **Wire it in:** `sindri compile` loads the full reachable graph (not just the entry module) before building and logs each loaded module; cycle and non-library errors abort the real compile. (Execution still runs only the entry module's tasks until 5c.)

### Phase 5c — Cross-module ordering and execution

Goal: `sindri compile` actually builds the entry module *and* its local library dependencies, in
dependency-first order.

- [x] Reintroduce task-graph edges — this time *consumed by the executor* — for cross-module ordering: add edges from every task of an upstream module to the first occupied step of each downstream module (DESIGN §7 step 3). Within a single module, step ordering stays positional; these are the first edges the executor actually reads.
  - [x] Test: all tasks of an upstream module precede the first-step tasks of a downstream module
  - [x] Test: independent modules (no dependency path) have no edge between them (parallel-eligible, even though the MVP runs them sequentially)
- [x] **Wire it in:** execute the multi-module graph honouring the edges — a module is fully built before any module that depends on it — so `sindri compile` on a module with a local dependency builds both.

### Phase 5d — Native-tool projection (Go)

Goal: a real multi-module Go build resolves its local dependencies, because Sindri generates Go's
local-dependency view before invoking `go build`.

- [x] Generate Go's local-dependency view (`go.work`, or `replace` directives) from the declared `{ module = … }` dependencies, so the declarations remain the single source of truth and cannot drift from a hand-maintained file (DESIGN §6)
  - [x] Test: a workspace with a local module dependency produces a `go.work` covering both module directories
  - [x] Test: removing the declaration removes the generated entry
- [x] **Wire it in:** write the generated `go.work` at the workspace root before the Go compile runs, so `go build` in a dependent module finds its local dependency.
  - [x] Integration test: `sindri compile` in a module that depends on a local Go library builds successfully
- [x] **Example:** add a multi-module example under `examples/` — a workspace whose executable module declares a `{ module = … }` dependency on a local Go library module in a sibling directory — and confirm `sindri compile` builds it end-to-end. Update `examples/README.md` (drop the "multi-module … not implemented yet" note) and the examples `justfile`.

### Phase 5e — Multi-module incrementality

Goal: a second `sindri compile` on an unchanged multi-module tree is silent, and editing a
dependency rebuilds its dependents.

- [x] A module tracks only its own source (its `**/*.{go,c,h,…}` superset glob + `go.mod` / `go.sum`, relativized); a dependent rebuilds when its own inputs change **or** a dependency module was rebuilt, carried by the cross-module edges above. Dependency files are not flattened into the dependent's input set (external deps stay covered by `go.mod` / `go.sum`).
  - [x] Test: changing a file in a local dependency marks the dependent module dirty (via the dependency edge)
  - [x] Test: each module's tracked files are recorded by workspace-relative path (state survives a workspace move)
- [x] **Wire it in:** the dependency-rebuilt signal is propagated during a real `sindri compile`, so the multi-module incremental behaviour holds end-to-end.
  - [x] Integration test: a second multi-module `sindri compile` on an unchanged tree is silent; editing the dependency's source re-runs the dependent

### Phase 5f — Shared Nickel resolution

Goal: a task script's Nickel source — and everything it transitively imports — is read from disk
at most twice per `sindri compile` (once across every task's definition-hash pass, once across
every task's evaluation pass), regardless of how many tasks share it. Verified via `DummyRuntime`'s
existing `read_count` tracking (the same mechanism `metadata_cache.rs`'s own tests already use to
prove a file is read only as many times as it's actually invalidated), not by giving a shipped
script an import it has no real use for — see Phase 8 for exercising this against a real plugin.

`TransitiveSource` already reports workspace-relative paths (fixed at the source, in Phase T5, so
a definition hash never depends on where the workspace is checked out) — no separate step needed
here for that.

- [x] Resolve a task's script — both for its definition hash and for evaluation — entirely within
  the serial planning phase (`resolve_task_plan`), for every task the dirtiness check finds dirty.
  `TaskPlan` carries the resolved `Vec<Command>` alongside its `Dirtiness`; `run_misses`/`run_one`
  run the already-resolved commands instead of calling `Script::evaluate` again. No Nickel type
  ever needs to cross into a worker thread, since only the resolved `Vec<Command>` — plain owned
  data — does
  - [x] Test: a dirty task's script is evaluated exactly once per `sindri compile`
- [x] Keep two long-lived `CacheHub`s across the whole planning phase — one reused for every
  task's definition-hash pass, one reused for every task's evaluation pass — instead of building a
  fresh hub per `resolve_hermetically` call. Each hub's own `id_of` cache-hit path already dedupes
  a file imported by more than one task; deliberately *not* merging the two hubs into one avoids a
  `SourceCache::add_string` collision (the hash-document and eval-document for the same task
  currently share one synthetic `script_path`), at the cost of one extra parse per uniquely
  imported file — a CPU-only, no-I/O cost judged not worth the collision risk
  - [x] Test: `runtime.read_count(...)` for a helper imported by two different tasks is 1 within the
    hash pass, and 1 within the evaluation pass
- [x] Share a plain `HashMap<AbsoluteFile, String>` of already-read file contents between the two
  hubs (populated by whichever pass reads a file first, consulted by both before either calls
  `file_system.read_to_string`), so the disk read itself — the expensive part — collapses to
  exactly once per file for the whole run; each hub still parses its own copy
  - [x] Test: `runtime.read_count(...)` for a helper imported by both a hash pass and an eval pass
    is 1 for the whole `sindri compile`
- [x] Scope `resolve_transitive_source`'s definition-hash input to exactly the `FileId`s reachable
  from the script's own document — via `resolve_imports`'s `resolved_ids` — rather than its hub's
  entire `file_paths` table; otherwise the hash-pass hub (now shared across every task) makes every
  task's definition hash depend on every other task's files, and on the order tasks were resolved in
  - [x] Test: with a shared hash-pass hub, resolving task B's definition hash after task A does not
    pull task A's script or imports into task B's hash

### Phase 5g — Module tools (workspace-built tools)

Goal: a task can consume a binary produced by an `executable` module in the same workspace, and that
mechanism is actually exercised — not just defined.

- [x] Support `module_tools` on modules (a module-level `sindri.build` field for now — no per-task Nickel declaration surface exists before Phase 8): a module may name an `executable` module in the same workspace via a `//path:binary` label; that module is built up to and including `package` before any task referencing it runs, and the resolved absolute path is handed to the referencing task's script directly via `inputs."module-tools".<binary>` (a hermetic data lookup, not a `PATH` search — DESIGN §6, mechanism adjusted from the literal write-up)
  - [x] Test: a task with a `module_tools` entry runs only after the referenced module is built
  - [x] Test: referencing a non-`executable` module in `module_tools` is an error
  - [x] Test: a binary still missing after the tool module is built fails with a named error
- [x] **Wire it in:** Sindri synthesizes a `generate`-step task per `module_tools` entry that runs the resolved binary, so the ordering, direct-path resolution, and error handling all run through the real `Lifecycle::run_compile` path, not tests alone.
- [x] **Example:** add an example under `examples/` where a module's task consumes a binary produced by a local `executable` module (e.g. a small code generator built by Sindri and run during `generate`), and update `examples/README.md` and the examples `justfile`.

---

## Phase 6 — Init commands

Goal: `sindri init workspace` and `sindri init module` scaffold the files needed to start a new project or add a new module, so users never have to write `sindri.workspace` or `sindri.build` from scratch.

- [ ] Add `sindri init workspace` subcommand: create a `sindri.workspace` file in the current directory with sensible defaults; error if one already exists
  - [ ] Test: creates a valid `sindri.workspace` that passes the Nickel contract
  - [ ] Test: returns an error if `sindri.workspace` already exists
- [ ] Add `sindri init module` subcommand: create a `sindri.build` file in the current directory with sensible defaults for the detected or specified language; error if one already exists
  - [ ] Test: creates a valid `sindri.build` that passes the Nickel contract
  - [ ] Test: returns an error if `sindri.build` already exists
- [ ] Both commands print what they created and suggest the next step
  - [ ] Test: output contains the created filename and a follow-up hint

---

## Phase 7 — Basic BSP

Goal: VS Code (with a BSP client) can import a Sindri workspace, see the build target, and receive inline `go build` diagnostics.

- [ ] Research and select a JSON-RPC-over-stdio library (or hand-roll the thin transport layer)
- [ ] Add `sindri bsp` subcommand; when invoked, start the BSP server loop (reads from stdin, writes to stdout)
- [ ] Handle `build/initialize` and `build/initialized` handshake
  - [ ] Test: server responds to `build/initialize` with the expected capabilities
- [ ] Implement `workspace/buildTargets`: return one target per discovered module
  - [ ] Test: response contains the expected build target for a single-module workspace
- [ ] Implement `buildTarget/sources`: return source directories derived from the Go plugin's input globs
  - [ ] Test: response contains the expected source root for a Go module
- [ ] Implement `buildTarget/compile`: run `go build`, parse its error output (file, line, column, message), emit `PublishDiagnostics` notifications
  - [ ] Test: a Go file with a compile error produces a `PublishDiagnostics` notification with the correct file, line, and message
- [ ] Integration test: import workspace in VS Code via BSP, trigger compile, verify diagnostics appear inline

---

## Phase 8 — Custom tasks, so the Go plugin exercises what's actually been built

Goal: a workspace author can declare their own task from Nickel — not just the Go plugin's fixed,
hardcoded set — and Sindri's language-integration surface (imports, parameters, contracts) is
exercised by a real, user-authored build, not only unit-test fixtures. Every task today comes from
`GoPlugin::tasks(artifact_type)`, a fixed Rust function; `module.ncl`'s only per-module
customisation is `parameters` (§6). This phase is what makes Phase 5f's caching work provable
against something a real user would actually write, rather than only against synthetic scripts.

At this stage of Sindri's evolution, a custom task's Nickel files are provided directly as part of
the workspace — checked in like any other source, conventionally under a `plugins/` directory —
not fetched, versioned, or distributed. No package manager, no plugin registry: that's the
"External plugin distribution" question DESIGN-TASK.md already scoped out, and stays out of scope
here too.

- [ ] Design a task-declaration contract on `module.ncl` (or a sibling), shaped around the same
  fields `Task::new` already takes: name, script, declared/managed input patterns, output pattern,
  declared parameters. A `Deserialize` path converts a validated declaration into `task::Task`,
  reporting a bad declaration through the same contract-error path as every other build-definition
  error.
  - [ ] Test: a valid custom task declaration deserializes into an equivalent `task::Task`
  - [ ] Test: a declaration missing a required field is a contract error naming it
- [ ] Support a script backed by a real file, not just an inline string: when a task's script names
  a `.ncl` file under the workspace's `plugins/` directory (rather than embedding Nickel source
  directly in `sindri.build`), that file's own path becomes its `script_path` outright — the same
  synthetic-location machinery `Task::script_path` uses for shipped scripts is unnecessary here,
  since the file genuinely exists — so its own relative imports resolve exactly the way the
  existing workspace-local-helper tests already prove, without inventing a new mechanism.
  - [ ] Test: a file-backed script under `plugins/` that imports a sibling `.ncl` file resolves and
    runs correctly through a real `sindri compile`
- [ ] Decide where a custom task attaches in the lifecycle graph — which `Step`, and how it coexists
  with the Go plugin's own tasks in the same module (`lifecycle.rs`'s `TaskGraphBuilder`).
  - [ ] Test: a custom task bound to a step runs in the correct order relative to the Go plugin's
    own tasks in that step
- [ ] **Example:** a workspace where a module declares a custom task whose script imports a small,
  workspace-local Nickel helper — the import doesn't need to be functionally load-bearing, just
  genuine and exercised by a real build — and update `examples/README.md` and the examples
  `justfile`.
