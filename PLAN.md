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

- [ ] Implement task executor: run a shell command, stream stdout/stderr to the terminal
  - [ ] Test: a succeeding command exits 0 and its stdout is captured
  - [ ] Test: a failing command exits non-zero and its stderr is captured
- [ ] Handle task failure: report via `miette` with the failing command and its output, exit non-zero
  - [ ] Test: task failure produces a `miette` error with the command and output in the message
- [ ] Wire the `compile` subcommand to execute the task graph up to and including `compile`
- [ ] Implement parallelism: tasks within the same lifecycle step with no dependency edges run concurrently
  - [ ] Test: two independent tasks in the same step complete in less time than their sequential sum

### Progress output

- [ ] Print a start line when each task begins: `  → <task-name>` (or similar)
- [ ] Print a completion line when each task finishes, including elapsed time: `  ✓ <task-name> (0.4s)` on success, `  ✗ <task-name> (0.4s)` on failure
- [ ] Add `--quiet` / `-q` flag: suppress all progress output; only errors are shown
  - [ ] Test: `--quiet` produces no progress lines on a successful build
  - [ ] Test: `--quiet` still shows errors on a failed build
- [ ] Task stdout/stderr is shown only on failure by default; shown always under `--verbose` / `-v`
  - [ ] Test: a passing task produces no stdout/stderr output in the terminal by default
  - [ ] Test: a passing task's output is shown with `--verbose`
  - [ ] Test: a failing task's output is always shown regardless of verbosity
- [ ] Progress output and tracing output never interleave on the terminal

### Telemetry

Format and rationale: DESIGN.md §7 (Telemetry).

- [ ] Add `serde_json` dependency
- [ ] Define a `TraceEvent` struct serialising to the Chrome trace `X` event shape (`name`, `ph`, `ts`, `dur`, `pid`, `tid`, `args`)
- [ ] Record one event per executed task: `ts` = µs from process start, `dur` = wall-clock duration in µs, `tid` = executor thread index
- [ ] Write `<build_dir>/telemetry.json` at build end (success or failure), wrapped in `{"traceEvents": [...]}`
  - [ ] Test: a build produces `telemetry.json` containing one event per executed task
  - [ ] Test: all event `dur` values are positive
  - [ ] Test: parallel tasks on different threads have distinct `tid` values

- [ ] Integration test: `sindri compile` in a valid Go module runs `go build` and exits 0

---

## Phase 4 — Incremental correctness

Goal: A second `sindri compile` on an unchanged tree skips all tasks immediately.

### Glob traversal

- [ ] Add `globset` and `ignore` dependencies (BurntSushi / ripgrep author)
- [ ] Configure `WalkBuilder` with `standard_filters(false)` to disable `.gitignore`, hidden-file filtering, and global ignore files — Sindri controls traversal explicitly
- [ ] Configure `WalkBuilder` with `follow_links(false)` (the default) — symlinks are not supported in the MVP
- [ ] Implement glob expansion: extract the literal prefix before the first wildcard and start `WalkBuilder` traversal there, so unrelated trees are never entered
  - [ ] Test: a glob of `src/**/*.go` matches `.go` files under `src/`
  - [ ] Test: a glob of `src/**/*.go` does not match files outside `src/`
  - [ ] Test: a glob of `src/**/*.go` does not traverse directories outside `src/`
- [ ] Design the glob input type to support both include and exclude patterns from the start (even if only includes are used in the MVP)
  - [ ] Test: an exclude pattern prevents a matching file from being returned

### Hashing

- [ ] Add `blake3` dependency
- [ ] Implement file hashing with cross-platform determinism guarantees:
  - Normalize CRLF → LF before hashing so a file checked out on Windows produces the same hash as on Linux
  - Normalize all paths to `/`-separated strings relative to the workspace root before hashing — never feed OS-native paths into the hash function
  - Sort file lists lexicographically before hashing — filesystem readdir order is not guaranteed
  - Hash raw file bytes only (after line-ending normalization) — no timestamps, permissions, executable bit, or other OS metadata
  - [ ] Test: a file with CRLF line endings produces the same hash as the same file with LF line endings
  - [ ] Test: the same set of files produces the same hash regardless of the order they are provided in
  - [ ] Test: changing any byte in a file changes its hash
- [ ] Include the task declaration itself (command, globs) in the hash so plugin changes force a re-run
  - [ ] Test: changing the task command changes the combined hash

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

- [ ] Add `rmp-serde` dependency (MessagePack — binary, compact, serde-compatible, platform-independent)
- [ ] Define the state data structures: per-task record of input file paths + hashes, output file paths + hashes, and task declaration hash
- [ ] Implement the `.target/<qualifier>/<step>/<task>/` directory layout
- [ ] Load a task's persisted state from its own directory on startup (missing state = task is dirty)
  - [ ] Test: a task with no state file is considered dirty
- [ ] Dirtiness check: compare current input and output hashes against persisted state; skip clean tasks
  - [ ] Test: a task whose inputs and outputs are unchanged is considered clean
  - [ ] Test: a task with a changed input file is considered dirty
  - [ ] Test: a task with a missing output file is considered dirty
  - [ ] Test: a task with a changed output file is considered dirty
  - [ ] Test: a task whose declaration has changed is considered dirty
- [ ] Persist state to a task's own directory after successful completion
  - [ ] Test: after a successful run, the task is considered clean on the next check
- [ ] Add `sindri state dump` subcommand: reads a task's `state.bin`, writes JSON to stdout for human inspection
  - [ ] Test: `sindri state dump` output is valid JSON containing the expected fields

### Telemetry

- [ ] Annotate each telemetry event's `args` with `{"cache": "hit"}` or `{"cache": "miss"}` based on the dirtiness check result
  - [ ] Test: a cache-hit task has `args.cache == "hit"` in `telemetry.json`
  - [ ] Test: a cache-miss task has `args.cache == "miss"` in `telemetry.json`

- [ ] Integration test: second `sindri compile` on an unchanged tree prints nothing to do and exits immediately

---

## Phase 5 — Init commands

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

## Phase 6 — Basic BSP

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
