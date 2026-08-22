# Coding Guidelines

These are the conventions for Rust code. They favour **precision over brevity**:
code should say exactly what it means, and the type system should make incorrect
states unrepresentable.

## 1. Naming

- **No abbreviations.** Write full words: `error` not `err`, `buffer` not `buf`,
  `source` not `src`, `context` not `ctx`, `message` not `msg`, `index` not `idx`,
  `length` not `len`, `expression` not `expr`, `argument` not `arg`,
  `directory` not `dir`.
- **A name must add information the type does not already give.** If a field of
  type `AbsoluteFile` is named `absolute`, the name is useless — it restates the
  type. Encode the *role*, *reference frame*, or *domain* instead:
  `disk_path` / `workspace_path` (which root each is anchored in), not
  `absolute` / `relative`.
- **Prefer the precise domain term.** A domain configuration file is a `ConfigFile`, not
  a `SourceFile`; a lifecycle position is a `Step`, not a `String`. Don't reach for
  a broad word (`data`, `value`, `source`) when the concept has a specific name in
  this codebase.
- **Naming is worth deliberating over.** A new type or field name is a design
  decision, not an afterthought — weigh the candidates and the reasoning, in review
  if need be, rather than settling for the first thing that compiles.

## 2. Types and Encapsulation

- **No `pub` (or `pub(crate)`) fields.** Expose state through accessor methods so
  storage can evolve without breaking call sites. Only make public what callers
  genuinely need.
- **Prefer newtypes and richer std types over primitives** for domain values.
  Use `Duration`, not `u64` micros. Use `Fiber(usize)`, `Stdout(Vec<u8>)`,
  `Step(SmolStr)` when a value carries domain meaning a primitive would erase.
  This also defends against positional-argument swaps at call sites.
- **Make illegal states unrepresentable by construction.** Standard domain path types
  are an example: `AbsoluteFile` / `AbsoluteDirectory` / `RelativeFile` /
  `RelativeDirectory` carry their invariant in the type, and an absolute path is
  only ever produced by resolving a relative one against a `WorkspaceRoot`. A raw
  `PathBuf` becomes a typed path at the edge, so everything downstream is typed.
- **Move functionality onto the wrapper type.** Single-stream operations belong on
  the type (`Stdout::to_string_lossy`); cross-field operations belong on the owning
  struct (`CommandOutput::combined_output`).
- **Derive `Default`** when an empty/zero value is meaningful (`Stdout::default()`
  beats `Stdout::new(Vec::new())`).
- **Push domain types through to serialization boundaries** with
  `#[serde(transparent)]` or `#[serde(serialize_with = …)]`, rather than
  converting to primitives in the caller.
- **This is not a license for premature abstraction.** One-off helpers and
  wire-format constants do not earn a newtype. Apply judgment, and introduce these
  improvements as separate, focused changes rather than bundling them into
  unrelated work.

## 3. Imports and Code Organization

- **Import types and functions by name, not their module.** `use std::io::Write;`
  then `Write`, not `use std::io;` then `io::Write`. `use std::fs::create_dir_all;`
  then `create_dir_all(path)`. Module-qualified call sites read as "code path", not
  "what is being done".
- **Don't fully-qualify types in code bodies.** Add a `use` and reference the short
  name. Never prefix with `::std::`; use `std::`.
- **Give every free function a typed home.**
  - If it constructs or finds a `T`, make it an associated function on `T`:
    `WorkspaceRoot::find(start)`, `BuildFile::find(workspace, fs)`,
    `Module::load(file)`.
  - If a group of related functions has no obvious owner, create a unit-struct
    aggregation: `Compiler::evaluate(…)`, `Telemetry::write(…)`. A zero-sized marker
    earns its keep by giving the group a name and namespace.
- **Resolve name collisions with aliases, not module qualification:**
  `use std::io::Result as IoResult;`, `use miette::Result as MietteResult;`,
  `use std::fmt::Result as FmtResult;`, `use std::io::Error as IoError;`.

## 4. Style

- **Explicit type annotations on every `let` binding**, including `let mut` and
  closure parameters where they aid clarity: `let workspace: Workspace = …`.
- **No unnecessary blank lines.** No blank line between consecutive `let` bindings
  or sequential statements unless there is a real logical break between sections.
- **No blank lines between `use` statements** — ever. Write them consecutively
  regardless of std / external / internal grouping. Stable `rustfmt` does not
  insert them.
- `rustfmt` is authoritative: `max_width = 120`, `reorder_imports = true`.

## 5. Errors

- Errors are domain-specific enums deriving `thiserror::Error`.
- Surface errors rather than hiding them. A discarded `Result` (`let _ = …`) needs
  a comment explaining why the failure is safe to ignore.

## 6. Testing

- Unit tests live in a `#[cfg(test)] mod tests` at the bottom of each file;
  end-to-end CLI behaviour lives in `tests/cli.rs`.
- Prefer in-memory mocks and hermetic test harnesses for tests. Reach for the real
  filesystem / process execution only when the test specifically covers that real
  behaviour (e.g. actual process spawning, genuine parallelism).
- Side effects go through traits so they can be stubbed — don't call `std::fs` or
  `std::process` directly in core library logic.
- **Keep functions small, especially where branching is involved.** A function with
  several conditional paths is hard to exercise exhaustively as a whole. Extract the
  branching logic into its own function so each path can be tested directly, with a
  minimal, focused setup — rather than constructing an elaborate scenario just to
  steer execution down one branch of a larger function.
- **It is fine, and often correct, to reshape the code under test to make testing
  simpler.** If reaching a particular branch or edge case demands an elaborate setup,
  the fix is usually the code, not the test: extract a function, invert a condition,
  parameterize a dependency. Unit tests should stay as simple as possible; treat one
  that requires a complicated scenario as a signal to refactor the code under test.
- **Test the edge cases of cardinality, not just the typical case.** For anything
  that operates over a collection or count, cover zero, one, and many — not only
  the middle case (e.g. "exactly one item"). Zero and many are frequently distinct
  code paths in disguise (an empty result, separators between multiple entries) and
  each deserves its own test.

