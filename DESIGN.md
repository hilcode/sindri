# Sindri (`sindri`) — Design Document

## 1. Overview

Sindri (CLI: `sindri`) is a build tool designed to combine the best properties of existing build systems while avoiding their principal failure modes:

- **Bazel/Buck**: excellent task graph, hermetic builds, remote caching — but extremely complex to use and configure.
- **Mill/SBT/Gradle**: fine-grained incrementality — but every build is a custom Scala/Groovy program, making each project a one-off.
- **Maven**: good lifecycle model, declarative — but verbose XML, plugin interactions are opaque, and the ecosystem is a patchwork.

Sindri targets the intersection: a **declarative**, **lifecycle-driven**, **incrementally correct** build tool with a clean plugin model and first-class IDE integration.

### Design principles

1. **Declarative over imperative.** Build files describe *what* a module is, not *how* to build it. Logic lives in plugins, not in build files.
2. **Explicit over implicit.** Module dependencies, plugin ordering, tool requirements — all declared, never inferred.
3. **Correct by default.** The build tool tracks inputs and outputs precisely enough to skip work safely and to detect when a rebuild is necessary.
4. **Recoverable.** The build tool knows enough about its environment (sources, generated files, cached outputs) to recover from any build failure or manual intervention without a full clean rebuild.
5. **Simple to extend.** Adding support for a new language or tool is done through a plugin. The plugin API is typed and the build tool actively helps plugin authors get it right.
6. **IDE-first.** BSP (Build Server Protocol) support is a core feature, not an afterthought.

---

## 2. Terminology

| Term | Meaning |
|------|---------|
| **Workspace** | The root of a project tree. Contains one or more modules. |
| **Module** | A single buildable unit, defined by a `sindri.build` file. |
| **Lifecycle** | An ordered sequence of named steps (e.g. `compile`, `test`). |
| **Step** | A named position in the lifecycle. Each step runs zero or more tasks. |
| **Task** | A unit of work contributed by a plugin. Bound to a lifecycle step. |
| **Plugin** | A Nickel package that contributes tasks, lifecycle steps, or both. |
| **Artifact** | A versioned, packaged build product — either produced by a module in this workspace or fetched from an external repository (e.g. a JAR from Maven Central). |
| **Target** | A BSP concept: a module+configuration pair (e.g. `my-lib [main]`, `my-lib [test]`). |

---

## 3. Workspace

A workspace is rooted at a directory containing a `sindri.workspace` file. This file declares workspace-level settings shared across all modules:

```nickel
# sindri.workspace
{
  name          = "my-project",
  sindri_version    = "0.1.0",
  build_dir     = ".target",
  plugins       = [
    { name = "sindri-java",   version = "1.0.0" },
    { name = "sindri-kotlin", version = "1.0.0" },
  ],
  repositories  = [
    "https://repo.maven.apache.org/maven2",
  ],
}
```

Credentials for private repositories must never appear in committed build files. Each plugin is responsible for obtaining secrets through environment variables or a credentials file that is never committed to version control. `sindri` imposes no credential mechanism of its own.

### Module discovery

Module loading is demand-driven. When `sindri` is invoked, it locates the entry-point module by walking up from the current directory until it finds a `sindri.build` (or `sindri-<qualifier>.build`) file, stopping at the workspace root. It then reads that file and follows all declared `module` dependencies transitively. Only modules reachable from the entry point are loaded — no filesystem walk is performed.

The explicit `module` dependency declarations in `sindri.build` files are therefore the complete description of the build graph. Modules not reachable from the requested entry point are never loaded.

---

## 4. Modules

Each module is defined by one or more build files in the same directory. The standard file is `sindri.build`. When a directory contains more than one module — whether because of multiple languages or multiple artifact types — each module gets its own build file named `sindri-<qualifier>.build`, where the qualifier is any identifier that distinguishes it. Each such file defines exactly one module.

### Module identity

A module's identity is its path relative to the workspace root, using `//` as the separator:

- `//libs/common` — the module defined by `libs/common/sindri.build`
- `//libs/common [kotlin]` — the module defined by `libs/common/sindri-kotlin.build`
- `//tools/codegen [bin]` — the module defined by `tools/codegen/sindri-bin.build`

### Artifact type

Every module declares exactly one artifact type. The type is a fixed vocabulary owned by the tool — it describes the module's role in the build graph, not its language or packaging format. Each type's capabilities are inherent and uniform across all languages:

| Type | Can be a `dependency` | Can be a `module_tool` | Description |
|------|:-:|:-:|-------------|
| `library` | ✓ | ✗ | Reusable artifact consumed by other modules during compilation |
| `executable` | ✗ | ✓ | Executable artifact (compiled native binary, JVM launcher, shell script, etc.); `package` places it in `${build-dir}/bin/`; exact form is plugin-defined |
| `web-archive` | ✗ | ✗ | Deployable web application |
| `container-image` | ✗ | ✗ | OCI/Docker image |

Plugins extend the *language axis*, not the type axis. A plugin declares which `(language, type)` pairs it supports — for example, the `sindri-java` plugin handles `(java, library)`, `(java, executable)`, and `(java, web-archive)`. When `sindri` encounters a module, it resolves the responsible plugin from the combination of `language` and `type` declared in the build file. Every plugin follows the same resolution rules; there are no built-in or default plugins that receive special treatment.

A module that needs to produce two artifact types (for example, source code that should be published both as a `library` and packaged as a `container-image`) is split into two build files in the same directory. The second module declares an explicit dependency on the first:

```
services/auth/
  sindri.build          # type = "library"  (compiled, tested, published to artifact repo)
  sindri-image.build    # type = "container-image"  (depends on //services/auth, packages it)
```

### Minimal module declaration

```nickel
# libs/common/sindri.build
{
  name     = "common",
  language = "java",
  type     = "library",
  version  = "1.0.0",

  dependencies = {
    compile = [
      { artifact = "example-org:some-lib" },
    ],
  },
}
```

### Inter-module dependencies

Dependencies on other modules in the same workspace are declared explicitly using their module path. Only modules with artifact type `library` may appear in `dependencies`; referencing an `executable`, `container-image`, or other non-library type as a dependency is an error.

```nickel
dependencies = {
  compile = [
    { module   = "//libs/common"          },
    { artifact = "example-org:some-lib" },
  ],
  test = [
    { module   = "//libs/test-helpers"                        },
  ],
},
```

Glob-based file tracking within a module is implicit (see §7). Cross-module ordering is derived entirely from explicit `module` dependency declarations, not from file glob overlap.

### Parameter values

A module supplies values for the parameters (§6) its tasks' scripts declare, keyed first by owning plugin then by parameter name:

```nickel
parameters = {
  "sindri-go" = { mode = "release" },
},
```

Every parameter a task's script declares must have a value here — there is no implicit default. Two modules built from the same sources but with different parameter values coexist side by side in the build directory rather than overwriting one another, since the resolved values are hashed into the binding hash that names each task's output location (§7).

### Inheriting from a parent

Build files can import shared declarations from the workspace root or from intermediate parent directories. A common pattern is a root `sindri.ncl` file:

```nickel
# sindri.build
let defaults = import "../../sindri.ncl" in
defaults & {
  name         = "my-service",
  dependencies = defaults.dependencies & {
    compile = defaults.dependencies.compile @ [
      { artifact = "com.example:extra-lib" },
    ],
  },
}
```

The `&` operator merges records and `@` appends to lists — both standard Nickel operations. Extending a single scope requires merging into the `dependencies` record and appending to the target scope's list.

---

## 5. The Lifecycle

The lifecycle is a fixed, ordered sequence of named steps. It is owned by the build tool. Plugins may insert additional steps but cannot reorder or remove built-in ones.

### Default lifecycle

```
start → generate → format → compile → document → test-compile
      → lint → test → integration-test → package → publish → end
```

| Step | Description | Source-mutating? |
|------|-------------|:---:|
| `start` | Runs before every other step, for any invocation — the earliest point a task can bind to. | No |
| `generate` | Generate source or resource files. May write into the source tree. | Yes |
| `format` | Apply or check code formatting. May write into the source tree. | Yes |
| `compile` | Compile main sources. | No |
| `document` | Generate API documentation. | No |
| `test-compile` | Compile test sources. | No |
| `lint` | Run static analysis tools over main and test sources. | No |
| `test` | Run unit tests. | No |
| `integration-test` | Run integration tests. | No |
| `package` | Assemble distributable artifacts (JARs, binaries, etc.). | No |
| `publish` | Push artifacts to a remote repository or registry. The destination, credentials, and protocol are entirely plugin-defined. | No |
| `end` | Runs once the lifecycle's own last named step has actually run — i.e. only when the invocation reaches the end of the lifecycle, not when it stops at an earlier step. | No |

`start` and `end` are ordinary steps a task may bind to directly, not mere placeholders: a task bound to `start` runs before anything else, unconditionally, on every invocation; a task bound to `end` runs once the lifecycle's own last named step has run, so it never fires for an invocation that stops short of the full lifecycle. They are also the two anchor points a plugin references via `pre`/`post` when inserting a brand-new step at the very beginning or very end of the lifecycle (see "Extending the lifecycle", below) — running something before/after the whole lifecycle and inserting a step at either extreme are two different, non-contradictory uses of the same two sentinels.

### Source-mutating steps

`generate` and `format` are the two steps *intended* to write to the source tree — a convention every other step's task is expected to honor, not something Sindri enforces or sandboxes against. Nothing stops a task bound to a later step from writing to source too; the tool simply doesn't guarantee anything about when that write is noticed. Because `generate` precedes `format`, which precedes `compile`, any generated files are formatted and then compiled in a single build — no intermediate build is required.

Sindri hashes source files once, after both mutating steps finish, and reuses those hashes as the inputs for every step from `compile` onward within that same build (§7). This is what the convention protects: a task bound to a later step that writes to source anyway won't corrupt the current build, but its own edit won't be picked up until hashing runs again on the *next* invocation — the same kind of responsibility §10 already places on plugin authors for non-determinism, rather than a guarantee the tool actively checks.

### Targeting a step

Running `sindri <step>` executes the lifecycle up to and including the named step:

```
sindri compile           # runs: generate, format, compile
sindri test              # runs: generate, format, compile, document, test-compile, lint, test
sindri publish           # runs all steps
```

### Extending the lifecycle

A plugin that introduces a new lifecycle step declares exactly which existing step immediately precedes it (`pre`) and which immediately follows it (`post`):

```nickel
# A plugin adding a "verify" step between integration-test and package
{
  step = "verify",
  pre  = "integration-test",
  post = "package",
}
```

At startup, before any build work begins, `sindri` validates the full lifecycle graph in a single pass:

1. Both `pre` and `post` must name steps that exist in the current lifecycle (either built-in or added by another plugin earlier in the plugin list).
2. `pre` must immediately precede `post` at the time the plugin is evaluated.

If either condition fails, the build aborts immediately with a precise error:

```
Error: Plugin "sindri-verify" cannot insert step "verify" between
       "integration-test" and "package":
       "integration-test" does not immediately precede "package"
       in the current lifecycle — "sindri-coverage" has already inserted
       "coverage" between them.

       Current lifecycle order near the conflict:
         ... → integration-test → coverage → package → ...

       Fix: declare pre = "coverage" (or post = "coverage") instead,
            or reorder plugins in sindri.workspace.
```

Plugin order in `sindri.workspace` determines evaluation order during single-pass validation. A later plugin can reference a step introduced by an earlier plugin.

Multiple plugins may contribute tasks to the same lifecycle step. When they do, their tasks execute in the order the plugins are declared in `sindri.workspace`. This makes the declaration order the single source of ordering authority — no separate rank or dependency declaration between plugins is needed.

---

## 6. Tasks and Plugins

### Plugin structure

A plugin is a Nickel package. It contributes one or more task declarations conforming to the `Task` schema shipped with `sindri`. The build tool validates every plugin against this schema at startup.

```nickel
# A simplified illustration of a Go compile task declaration
{
  task    = "go-compile",
  step    = "compile",
  inputs  = [ "**/*.go" ],
  outputs = [ "**/*" ],
  tools   = [ "go" ],
  script  = fun inputs => [
    { program = "go", arguments = [ "build", "-o", inputs."output-directory", "./..." ] },
  ],
}
```

A task's commands are not a static field — they come from evaluating the task's `script` (below) against its bound parameters and resolved inputs.

### Scripts and Commands

A **Command** is a single runnable process invocation, executed directly with no shell:

```nickel
{
  program           = "go",                 # required — the executable
  arguments         = [ "build", "./..." ], # optional, default []
  environment       = { GOWORK = "…" },      # optional, default {}
  working-directory = "my-lib",             # optional, default: the module directory
}
```

`program` stands apart from `arguments` — rather than being their head — because it is the token `sindri` resolves and verifies against the available tools (Tool verification, below). `arguments` is always a list, never a shell string, so there is no quoting or shell expansion to reason about.

A **Script** is a Nickel expression that, given a task's bound parameter values and its resolved input file set, evaluates to a non-empty, ordered list of `Command`s. The expression is pure — it performs no I/O and cannot read the host environment. `sindri` supplies the parameter values and the matched files (it owns globbing); the expression only computes each command's argument vector, environment, and working directory.

This is the separation the task model rests on: the script decides *what* to run, as plain data; `sindri` performs the *running*. A script's identity for caching purposes (§7) is its **definition hash**, taken over the expression's source — together with everything it imports — never over the resolved argument vectors, environments, or directories it evaluates to, since those are only ever a function of already-hashed inputs.

### Parameters

A **Parameter** is a named build setting owned by the plugin that defines it, identified by `(plugin, name)` — so two plugins may each define, say, a `mode` parameter with no collision and no shared registry to conflict over. Its legal values are constrained by a **ParameterType**: a Nickel contract the plugin authors inline (`on/off`, `one_of("debug", "release")`, …), rather than one drawn from a shared type registry.

A script declares which parameters it requires as authored data, read without evaluating the script itself — so a task is forced to supply a value for every parameter its script requires. `sindri` validates a supplied binding against that declared set *before* it evaluates the script: a missing or ill-typed value is a build-definition error naming the task and the parameter, never a raw Nickel failure deep inside a plugin.

Parameters are what let a debug build and a release build — or a normal build and a coverage build — coexist in the build directory without overwriting each other (§7): the resolved parameter values are hashed into a **binding hash** that names each task's output location, so switching between parameter values never forces a full rebuild.

### Input and output file sets

Each task declares:

- a **declared** input — the build file's own glob of files it consumes ("where are my sources"),
- a **managed** input — a glob contributed by the plugin itself, for artifacts it generated and already knows the location of (e.g. a generated `go.work`); the user never writes this,
- one **output** — a glob of files the task produces, resolved against the task's own output directory (§7) once the script has run.

Each of these is a **FileSetPattern**: an ordered list of include-only globs. A file is a member iff it matches at least one glob — there are no excludes and no separate directory-scoping rules. A `FileSetPattern` is a plain description; it is resolved against the workspace into the concrete matched files only at build time. A task's effective input is the union of its declared and managed file sets — this is how a generated artifact such as `go.work` enters a task's fingerprint (§7) without the user's own glob ever needing to mention it.

All declared globs are tracked by file watchers (§9). When a file matching one changes, the task is marked dirty and will re-run on the next build.

### Task ordering

Task ordering is determined entirely by the lifecycle. All tasks bound to step N complete before any task in step N+1 begins. Cross-module ordering is governed by explicit `module` dependency declarations (§4). There is no implicit ordering derived from glob overlap — globs are used only for incremental correctness (determining which tasks are dirty), not for ordering.

### Tool verification

The `tools` field lists executables a task requires. At startup, `sindri` verifies that each listed tool is present in the Nix store (i.e. is available in the Devenv-managed shell). A missing tool causes an immediate, named failure before any build work starts.

### Plugin dependencies

Plugins can declare their own dependencies, independent of the modules that use them. These take two forms.

**Artifact dependencies** are external packages needed at task execution time. They are declared in the plugin package's own manifest and resolved through the same mechanism as module artifact dependencies (§8).

**Module dependencies** are the mechanism for workspace-built tools. A task can declare that it depends on a module within the same workspace — for example, a custom code generator that is itself built by `sindri`. This is expressed through a `module_tools` field, which is distinct from `tools` (Nix executables) because the verification mechanism differs: `module_tools` are verified to exist only after the referenced module has been built, not at startup. Only modules with artifact type `executable` may be referenced in `module_tools`.

```nickel
# A plugin task that uses a tool built within the same workspace
{
  task         = "my-codegen",
  step         = "generate",
  inputs       = [ "src/main/schema/**/*.schema" ],
  outputs      = [ "src/generated/**/*"          ],
  module_tools = [
    { module = "//tools/codegen", binary = "codegen" },
  ],
  script       = fun inputs => [
    { program = "codegen", arguments = [ "--input", "src/main/schema", "--output", "src/generated" ] },
  ],
}
```

When `sindri` encounters a task with `module_tools`:

1. The referenced module is built up to and including its `package` step before the task runs.
2. The named binary (produced by that module's packaging) is added to the task's executable search path.
3. If the binary cannot be found after the module is built, the build fails immediately with a named error.

Workspace-built tools are therefore first-class participants in the build graph. The ordering guarantee is strict: a module that produces a tool is fully built before any task that consumes it runs, regardless of which other modules declare a dependency on that plugin.

### The `Task` schema

The build tool ships a set of typed Nickel contracts as part of its core. `sindri` applies them automatically when it loads build files and plugins — no import is required. A task declaration — its script, its declared parameters, and its input/output patterns — is simply a plain Nickel record:

```nickel
{
  task = "my-task",
  ...
}
```

`sindri` validates it against the `Task` contract at startup. If a required field is missing or has the wrong type, `sindri` reports a precise error before any build work begins.

### Native build tools vs. low-level compilers

For languages with a capable native build tool — Go (`go build`) and Rust (`cargo`) being the primary examples — plugins invoke the native tool directly rather than the low-level compiler (`go tool compile`, `rustc`). For languages without a dominant native build tool (Java, C, C++), plugins invoke the compiler directly.

The dividing line between the two modes is the same one in every case:

> **Sindri owns knowledge and orchestration; the native tool owns the translation of source into object code.**

Sindri always owns the module graph, the dependency declarations, the lifecycle, dirtiness tracking, telemetry, and BSP. The native tool always owns compilation, linking, and its own build cache. The modes differ only in *how far* into the "translation" half Sindri reaches:

- **Native-tool mode** delegates dependency resolution, caching, and incremental *compilation* to the native tool, keeping plugins simple at the cost of reduced Sindri visibility into individual compilation units. It does **not** delegate Sindri's own responsibilities — see the responsibilities below.
- **Low-level mode** invokes the compiler per unit, so Sindri discovers inputs, feeds them to the compiler, and owns incrementality and caching at the task level. This buys full visibility and remote-cache eligibility, at the cost of re-deriving what the native tool would otherwise do (build-constraint evaluation, cgo, embedding, the package graph, linking).

Low-level mode is reserved for languages with no native build tool, and for the future case where remote caching or per-task visibility justifies the additional cost (§16). It is never required merely to make Sindri "do more" — native-tool mode already has Sindri owning the entire build graph.

#### Native-tool mode does not defer correctness to the tool

Choosing native-tool mode delegates *compilation*, not *correctness*. In this mode Sindri still:

- **Tracks dirtiness authoritatively** over its own view of the inputs, never trusting the native tool's cache to decide whether a Sindri task may be skipped. The MVP tracks a deliberate **platform-independent superset** of each module's source — a glob of every file type the tool might compile (for Go: `**/*.{go,c,h,cc,cpp,cxx,hh,hpp,hxx,m,s,S}` plus `go.mod`/`go.sum`) — and lets the native tool select among them. Over-inclusion only ever costs a spurious rebuild; it can never miss an input. A precise, per-target set via the tool's own introspection (Go's `go list`, which also surfaces `//go:embed` inputs) is a future refinement (§16).
- **Keeps state relocatable.** Discovery yields absolute paths; Sindri relativizes every tracked file against the workspace root before hashing or persisting, so a workspace can be moved or renamed without invalidating state (§7). A tracked file that does not resolve under the workspace root is an error, never a silent skip.
- **Treats the `sindri.build` declarations as the source of truth.** The inter-module dependencies declared in `sindri.build` (§4) — not the native tool's own configuration — define the build graph. Where the native tool needs its own view of local dependencies (Go's `go.work` / `replace` directives, for example), Sindri *generates* it from the declarations rather than reading a hand-maintained file, so the two can never drift.

### Plugin tooling

- `sindri plugin validate <path>` — validates a plugin package against the `sindri` schemas without running a build.
- `sindri plugin new <name>` — scaffolds a new plugin with the correct directory structure and a skeleton task declaration.

---

## 7. Build Execution

### Task graph construction

Before executing any step, `sindri` constructs a task graph for the requested lifecycle range:

1. Collect all tasks bound to steps within the range.
2. Add edges between all tasks in step N and all tasks in step N+1, for each step N in the range.
3. For each explicit inter-module dependency, add edges from all tasks in the upstream module to all tasks in the first step of the downstream module.
4. The resulting DAG determines execution order.

### Parallelism

Tasks with no dependency edges between them are eligible to run in parallel. `sindri` runs as many eligible tasks concurrently as there are available CPU cores (configurable).

### Resolution

A `Task` — its script, its declared and managed inputs, its output, and its declared parameters (§6) — is a **description**: on its own it holds no file lists, no parameter values, and no concrete commands. **Resolution** binds it for a concrete build:

- each parameter is bound to a value, validated against its `ParameterType` contract;
- each `FileSetPattern` is matched against the workspace to yield the concrete files — the input file sets before the script runs, the output file set after it runs;
- the script is applied to the bound parameters and the resolved (declared ∪ managed) input files to yield the ordered commands to actually run.

### Incremental correctness

`sindri` maintains a persistent record of three distinct hashes per resolved task — i.e. per `(task, binding-hash)` pair, so distinct parameter bindings (§6) have entirely independent dirtiness state. Conflating these three is the main hazard the model guards against:

1. **Definition hash** — did the build definition change? Taken over the task's name, its script's transitive source (the expression together with every file it imports), its input and output pattern hashes, its declared parameters, and a `(sindri, Nickel)` version salt, since both shape how the script evaluates. A bump of either version deliberately dirties every task.
2. **Content hash** — did a tracked file change, appear, or disappear? Computed separately for the resolved input file set and the resolved output file set, over each member file's path and content. This is the file-level dirtiness signal.
3. **Binding hash** — over the resolved parameter values, ordered by parameter (§6). Not a dirtiness signal: it names the output location, so different parameter bindings coexist side by side instead of overwriting one another:

   ```
   <build-dir>/<module>/<task-name>/<binding-hash>/
   ```

A resolved task is re-run when any of the following is true:

- its definition hash changed (the script's source, an import, a pattern, a declaration, or the Sindri/Nickel version),
- its resolved input file set is dirty (a tracked file's content changed, was added, or was removed),
- its resolved output file set is dirty (an expected artifact was modified or is missing — this guards against tampering or partial results).

A resolved task is skipped when all three are false.

If a task fails, its record is not persisted: on the next invocation, the mismatch between the persisted (absent, or stale) record and the freshly computed one is detected and the task re-runs automatically. No staging areas or rollback mechanism are needed — this makes the build self-correcting.

### Metadata cache

`sindri` keeps a per-file cache of content hashes, keyed by workspace-relative path: a tracked file's hash, once resolved, is reused by every later dirtiness check or run record that consults it, until something explicitly tells the cache that file may have changed. A file shared by more than one resolved input or output set — a common case, since sibling tasks routinely declare overlapping globs — is therefore read and hashed only as many times as it is actually invalidated, not once per consultation.

The cache is backed by one small persisted record per tracked file — never a single workspace-wide blob rewritten wholesale — so confirmed state survives a build getting interrupted before it finishes. Each record holds the file's size, modification time, and content hash as of the last time it was written. A lookup first takes a cheap `stat`-equivalent reading of the file's current size and modification time: if both still match the persisted record, its hash is trusted without reading the file's content at all; if the size differs, or the file is missing, the file is dirty without needing to read it either; only when the size matches but the modification time has moved does the file actually get read, to resolve the ambiguity — some VCS/checkout tools set every checked-out file's modification time to the checkout or commit time rather than leaving it to reflect a per-file edit, so two files can easily end up sharing one modification time despite differing content; this is a real case a build tool has to handle correctly, not just a theoretical one. Records are addressed by a hash of their own path rather than a mirror of the source tree, so lookups don't depend on — or expose — the tree's own layout, and records are spread evenly across a bounded set of subdirectories regardless of how unevenly the real tree is shaped.

A task's own output is the one thing its own run is guaranteed to change, and a dirtiness check may already have cached those files as missing or stale before the task ran — so `sindri` drops the in-memory (this-build) entry for every file in a task's resolved output as soon as that task finishes, before anything reads them through the cache again. This invalidation is deliberately soft: it never declares a file dirty by itself, it only forces the next read to be a real one, so a later task in the same build that only consumes this file — rather than running it — still sees an accurate hash and can correctly stay clean if the content it actually depends on turns out unchanged, even though the task that produced it did run. Once a task's resolved input and output are settled, `sindri` persists their fresh records so a later build can trust them via the cheap check instead of reading their content again.

### Caching

The build tool uses a content-addressable local cache: task outputs are stored keyed by a hash of all inputs. When a task would re-run, `sindri` first checks whether a cached output exists for the current input hash. If so, the cached outputs are restored without running the task.

The caching architecture is designed to support a remote cache backend (shared across machines and CI) as a future extension, without requiring changes to the core execution model.

### Telemetry

After every successful build, `sindri` writes `<build_dir>/telemetry.json` in the **Chrome trace format**, directly loadable in [Perfetto](https://ui.perfetto.dev) or `chrome://tracing` without additional tooling. A failed build writes no trace — it returns early on the failing task and is diagnosed from its error rather than its timeline, which keeps every `telemetry.json` a record of a whole build rather than an aborted fragment. This follows from the engine's return-early control flow; a future decoupled event sink (§11) could instead let a telemetry listener flush on build-end regardless of outcome, at which point this behaviour would likely change.

Each executed task produces one complete event (`"ph": "X"`, meaning start time and duration are recorded together):

```json
{
  "traceEvents": [
    { "name": "go-compile", "ph": "X", "ts": 0,      "dur": 412000, "pid": 0, "tid": 0, "args": {"cache": "miss"} },
    { "name": "go-test",    "ph": "X", "ts": 412000,  "dur": 821000, "pid": 0, "tid": 0, "args": {"cache": "miss"} }
  ]
}
```

| Field | Meaning |
|-------|---------|
| `name` | Task name |
| `ph` | Always `"X"` — a complete event encoding both start and duration |
| `ts` | Start timestamp in microseconds from process start |
| `dur` | Wall-clock duration in microseconds |
| `pid` | Always `0` (single process) |
| `tid` | Executor thread index; parallel tasks on different threads have distinct values, making concurrency visible as parallel lanes in the flame chart |
| `args` | `{"cache": "miss"}` on first run; `{"cache": "hit"}` when the task was skipped due to incremental correctness (Phase 4) |

Tasks skipped by the cache still emit an event (with `"cache": "hit"`), so the file is a complete, comparable record of every build regardless of how many tasks actually ran. This makes it straightforward to track cache hit rates and critical-path evolution over time.

---

## 8. Dependency Management

### Dependency scopes

Dependencies are grouped by scope. The four scopes and the build phases they are visible in:

| Scope | Compilation | Test compilation | Execution | Test execution |
|-------|:-:|:-:|:-:|:-:|
| `compile` | ✓ | ✓ | ✓ | ✓ |
| `test` | ✗ | ✓ | ✗ | ✓ |
| `test-runtime` | ✗ | ✗ | ✗ | ✓ |
| `runtime` | ✗ | ✗ | ✓ | ✓ |

The invariant: `runtime` and `test-runtime` dependencies are never visible during compilation. `test`-scoped dependencies are invisible to main-source compilation.

### Artifact dependencies

Dependencies are declared under a `dependencies` record with one key per scope. Unused scopes may be omitted.

```nickel
dependencies = {
  compile = [
    { artifact = "example-org:core-lib"     },
    { artifact = "example-org:util-lib"     },
  ],
  test = [
    { artifact = "example-org:test-support" },
  ],
  test-runtime = [
    { artifact = "example-org:test-server"  },
  ],
  runtime = [
    { artifact = "example-org:db-driver"    },
  ],
},
```

Module dependencies use the same structure:

```nickel
dependencies = {
  compile = [
    { module = "//libs/common"       },
  ],
  test = [
    { module = "//libs/test-helpers" },
  ],
},
```

Artifact and module dependencies may be mixed freely within a scope.

### Transitive visibility

By default, a module's dependencies are not visible to its consumers. If module A depends on B, a module that depends on A does not see B at compile time — it must declare B itself if it needs it.

The `export` scope declares compile dependencies that *are* re-exported to consumers. Its entries are compile-time inputs exactly like `compile` entries — the two scopes together are what the module compiles against — but `export` entries are additionally visible to modules that depend on this one:

```nickel
dependencies = {
  compile = [
    { artifact = "example-org:core-lib" },  # a compile-time input, not visible to consumers
  ],
  export = [
    { artifact = "example-org:api-types" },  # a compile-time input and visible to consumers
    { module   = "//libs/common"         },  # modules too
  ],
},
```

Re-export is a property of the compilation path only, so there is deliberately no exported counterpart to the `test`, `test-runtime`, or `runtime` scopes: an entry is re-exported precisely by being placed in `export` rather than `compile`.

There is no compile-only scope. Dependencies that are needed at compile time but not at runtime (e.g. annotation processors, symbol processors) are not module-level dependencies at all — they are declared in the plugin's own configuration namespace and placed on the appropriate path by the plugin:

```nickel
{
  name     = "my-service",
  language = "java",
  type     = "library",
  dependencies = { ... },
  java = {
    annotation_processors = [
      { artifact = "org.projectlombok:lombok" },
    ],
  },
}
```

These are resolved through the same lock file mechanism as regular dependencies. The plugin owns the distinction between compile-time inputs and processor path; the module just declares which processors it uses.

### Lock file

Artifact versions are not declared in build files. Instead, the workspace lock file (`sindri.lock`) is the single source of truth for every artifact version in the build. It is committed to version control.

```nickel
# sindri.lock (managed by sindri, do not edit manually except for overrides)
{
  resolved = [
    { artifact = "org.slf4j:slf4j-api",        version = "2.0.9",  sha256 = "abc123..." },
    { artifact = "org.slf4j:slf4j-impl",       version = "2.0.9",  sha256 = "def456..." },
    { artifact = "example-org:core-lib",       version = "3.1.0",  sha256 = "ghi789..." },
    ...
  ],
  overrides = [
    # Pin a specific version, overriding whatever the graph would resolve to:
    { artifact = "org.slf4j:slf4j-api", version = "2.0.9" },
  ],
}
```

The `resolved` section is written entirely by `sindri` and must not be edited manually. The `overrides` section is the only part users edit directly.

The lock file is currently workspace-level: one set of pinned versions applies to all modules. Module-level version overrides (e.g. a binary module pinning a different version of a library than the rest of the workspace) are a planned capability; the edge cases this introduces are deferred design work (see §15).

### Adding and updating dependencies

```
sindri deps add <artifact>[@<version>]   # add artifact; pins to version or resolves latest
sindri deps update [<artifact>]          # update one or all artifacts to latest
```

When adding or updating, `sindri` resolves the full transitive graph. For each artifact not yet in the lock file, if all transitive paths agree on a version, that version is recorded. If paths conflict, `sindri` reports an error and requires an explicit pin:

```
Error: Version conflict for "org.slf4j:slf4j-api":
  - 2.0.9  via //libs/core → ...
  - 1.7.36 via //libs/legacy → ...

Run `sindri deps add org.slf4j:slf4j-api@<version>` to pin a version.
```

Artifacts already recorded in the lock file are not re-resolved unless `sindri deps update` is run — the locked version is used as-is, overriding any transitive request for a different version.

---

## 9. File Watching

`sindri` uses the [`notify`](https://github.com/notify-rs/notify) crate for cross-platform file system event monitoring:

- **Linux**: inotify
- **macOS**: FSEvents
- **Windows (WSL2)**: inotify within the WSL2 environment

All input and output globs declared by tasks are registered with the file watcher at startup. The watcher is kept running for the lifetime of the `sindri` process (both in one-shot and watch-mode builds).

Globs are not re-evaluated by walking the file system on each build. Instead, the watcher tracks file creation and deletion events and updates the glob match set incrementally. A file newly created inside an input glob's pattern is immediately tracked as a potential build input.

### Watch mode

Running `sindri watch [<step>]` keeps the process alive. When a tracked file changes, `sindri` schedules a build of the affected modules (and any modules that depend on them) for the specified step. If a build is already in progress when a change arrives, the change is queued and a new build starts after the current one finishes.

When a source-mutating step (`generate`, `format`) writes files into the source tree, the watcher will observe those writes and schedule another build. If a `generate` step produces source files that `format` then modifies, `format`'s changes invalidate `generate`'s recorded output hash, causing another `generate` run — a rebuild loop. This is always avoidable: either (1) the code generator produces output that already conforms to the formatting rules, (2) generated files are excluded from the `format` step, or (3) generated output is directed to `${build-dir}` rather than the source tree. A rebuild loop indicates that none of these conditions hold and is a sign of misconfiguration. The investigative mode (see §16) helps diagnose unexpected rebuild behaviour.

---

## 10. Environment and Reproducibility

`sindri` delegates environment management entirely to [Nix](https://nixos.org/) and [Devenv](https://devenv.sh/). It does not manage toolchains, JVM versions, or compiler installations itself.

### Requirements

- A `devenv.nix` at the workspace root defines the build environment.
- All tools referenced in plugin `tools` declarations must be present in the Nix store (i.e. provided by the Devenv shell).
- `sindri` verifies tool availability at startup by checking `which <tool>` within the active Nix environment and confirming the resolved path is under the Nix store prefix (`/nix/store/…`). A tool found outside the Nix store (e.g. from the user's `PATH`) is rejected.

### Windows

Native Windows is not supported. Windows users must use WSL2 with a Nix-enabled Linux distribution.

### Reproducibility guarantee

Given the same `devenv.lock` (pinning the Nix environment) and `sindri.lock` (pinning artifact versions), any two machines with the same `sindri` version will produce byte-identical build outputs. `sindri` does not take responsibility for non-determinism within plugin commands (e.g. a plugin that embeds the current timestamp) — that is the plugin author's responsibility.

---

## 11. BSP Integration

`sindri` implements the [Build Server Protocol](https://build-server-protocol.github.io/) (BSP). Any BSP-aware IDE — IntelliJ IDEA, VS Code (via the BSP extension), Eclipse — can import a Sindri workspace without any tool-specific plugin beyond a BSP client.

### What `sindri` exposes over BSP

| BSP Query | `sindri` response |
|-----------|--------------|
| `workspace/buildTargets` | One target per module×configuration pair (main, test). |
| `buildTarget/sources` | Source directories derived from task input globs for the target. Generated source directories (output globs of `generate` tasks) are included as source roots. |
| `buildTarget/dependencySources` | Source JARs for artifact dependencies. |
| `buildTarget/resources` | Resource directories derived from resource input globs. |
| `buildTarget/scalacOptions` / `buildTarget/javacOptions` | Compiler flags for the target. |
| `buildTarget/classpath` | Resolved compile-scope dependencies for the target (main or test, as appropriate). |
| `buildTarget/compile` | Triggers an `sindri compile` or `sindri test-compile` and streams diagnostics. |
| `buildTarget/test` | Triggers an `sindri test` for the target. |
| `buildTarget/run` | Runs the module's main class. |

### Generated sources

When the `generate` step produces files into a source directory (e.g. `src/generated/java/`), the corresponding task's output glob is registered as a BSP source root. IDEs see generated files as part of the module's source set without requiring a build to have run first — `sindri` reports the expected output directory even if it does not yet exist.

### Engine event sink (consideration)

BSP and watch mode (§9) are inherently streaming: diagnostics are pushed as they are found and progress is reported live to the IDE. Today the build engine returns a `Vec<TaskOutcome>` that the caller post-processes (terminal progress, telemetry, state persistence). Once a second, streaming consumer (BSP) exists, consider having the engine instead emit events — `TaskStarted`, `TaskFinished{hit|miss}`, `TaskFailed`, `BuildFinished` — to a sink that interested parties observe. A terminal reporter, a telemetry writer, the state persister, and a BSP bridge each become independent listeners, and decisions currently baked into control flow — such as whether telemetry is written for a failed build (§7) — become a listener's concern instead. Prefer a simple synchronous observer trait (in the style of the `FileSystem` / `Runtime` traits) over a full asynchronous message bus until the IDE and watch use cases prove they need true fan-out.

---

## 12. Configuration Language

Build files (`sindri.build`, `sindri-<lang>.build`, `sindri.workspace`) are written in [Nickel](https://nickel-lang.org/), a typed configuration language designed for exactly this kind of structured, composable, validated configuration.

### Why Nickel

- **Gradual typing with contracts**: fields in plugin and module declarations are type-checked. Missing or malformed fields are reported with precise error messages.
- **Functional and composable**: records merge with `&`, lists append with `@`. Parent project defaults can be imported and overridden cleanly.
- **Deterministic**: no I/O, no side effects during evaluation. The same inputs always produce the same build description.
- **Separate from execution**: Nickel evaluates to a data structure. It does not run the build — `sindri` reads that data structure and runs the build. This keeps build files analyzable without executing them.

### Typed schemas

`sindri` ships a set of Nickel contracts as part of its core and applies them automatically — no import required in build files or plugins:

```
Module        — the shape of a sindri.build file
Workspace     — the shape of a sindri.workspace file
Task          — the shape of a plugin task declaration
LifecycleStep — the shape of a plugin lifecycle extension
ArtifactDep   — { artifact: String }
ModuleDep     — { module: String }
Dependency    — ArtifactDep | ModuleDep
Dependencies  — { compile?: Array Dependency, export?: Array Dependency,
                  test?: Array Dependency, runtime?: Array Dependency,
                  test-runtime?: Array Dependency }
```

`sindri` applies the appropriate contract when it reads each file: `Module` for every `sindri.build`, `Workspace` for `sindri.workspace`, `Task` for every plugin task declaration, and so on.

### IDE support

Nickel ships a language server, `nls` (Nickel Language Server). `sindri` provides LSP support for build files by shipping a VS Code extension and IntelliJ plugin that configure `nls` for `*.build` and `*.workspace` files and provide the built-in contracts as type information.

With this setup, editors provide type-aware completion, hover documentation, and inline error reporting for all build files — including plugin-contributed fields.

---

## 13. File Layout Reference

```
<workspace-root>/
  sindri.workspace          # Workspace declaration
  sindri.lock         # Artifact lock file (committed to VCS)
  devenv.nix            # Nix environment declaration
  devenv.lock           # Nix lock file (committed to VCS)

  libs/common/
    sindri.build            # Module declaration (language-agnostic or single-language)

  services/auth/
    sindri-java.build       # Java module in this directory
    sindri-kotlin.build     # Kotlin module in the same directory

    src/
      main/
        java/           # Java sources (matched by sindri-java.build input glob)
        kotlin/         # Kotlin sources (matched by sindri-kotlin.build input glob)
        resources/
      test/
        java/
        kotlin/
    .target/            # All build outputs (not committed to VCS)
      classes/
      test-classes/
      generated/
      telemetry.json    # Chrome trace file — open in Perfetto or chrome://tracing
```

---

## 14. CLI Reference (Sketch)

### Target selection

All build commands accept an optional module target argument. If omitted, `sindri` walks up from the current directory to find the nearest `sindri.build` file and uses that as the entry point.

| Target argument | Meaning |
|----------------|---------|
| *(none)* | Nearest `sindri.build` at or above the current directory |
| `//libs/common` | The specific module at that path |
| `//libs/common [kotlin]` | The specific qualified module at that path |

### Commands

| Command | Description |
|---------|-------------|
| `sindri <step> [<target>]` | Run the lifecycle up to and including `<step>` for the given target. |
| `sindri watch [<step>] [<target>]` | Run in watch mode; rebuild on file changes. |
| `sindri clean` | Delete the build directory (`.target/` by default) from the workspace root. |
| `sindri deps update [<artifact>]` | Re-resolve and rewrite the lock file. |
| `sindri plugin validate <path>` | Validate a plugin against `sindri` schemas. |
| `sindri plugin new <name>` | Scaffold a new plugin. |
| `sindri lifecycle` | Print the current resolved lifecycle order. |
| `sindri bsp` | Start the BSP server (invoked by IDEs, not users directly). |

---

## 15. Open Questions

Items deliberately deferred; they need a decision before implementation begins.

1. **Plugin distribution**: How are plugins versioned and fetched? A dedicated `sindri` plugin registry? Maven Central? Git references? This affects sindri.workspace's `plugins` field and the lock file format.

2. **Test result reporting**: How are test results reported? JUnit XML? A custom format? How does this integrate with BSP's test reporting protocol?

3. **Module version inheritance**: Can a workspace declare a default version for all modules and allow individual modules to override it?

4. **Module-level dependency version overrides**: The lock file is workspace-level, but individual modules may need to pin a different version of an artifact (e.g. a binary module depending on an older library than the rest of the workspace). The mechanism for declaring module-level overrides and the edge cases they introduce (two modules pinning different versions of the same artifact while a third depends on both) need careful design.

---

## 16. Future Work

Items agreed to be out of scope for the initial implementation.

- **Cross-task references**: A downstream task consuming an upstream task's output cannot statically know the upstream's binding hash (§7), so a plain glob cannot reach it. The planned mechanism is a symbolic reference, `//module/@task-name/file-name`, that `sindri` resolves to `<task-name>/<binding-hash>/file-name` at build time. Which upstream binding a reference selects is a validation rule, not a propagation rule: it is valid only if the downstream's parameter binding covers every parameter the upstream task requires, checked statically at build-definition load time — it does not let a reference select a *different* upstream binding (e.g. a release downstream depending on a debug upstream). Once available, this subsumes the current Phase 5e mechanism, where a rebuilt dependency forces its whole dependent module to rebuild wholesale rather than only the tasks that actually consume the changed file.
- **Verbose / investigative mode**: A mode (e.g. `sindri --investigate`) that logs which files triggered a rebuild, which tasks were marked dirty and why, and what the file watcher observed. Useful for diagnosing misconfigured globs or unexpected rebuild loops.
- **Local and remote caching**: Store task outputs in a content-addressable cache (keyed by input hash) so that reverting a change restores outputs without re-running the task. The same structure supports a remote cache backend shared across machines and CI. Most valuable once Sindri invokes low-level compilers directly — native tools like `go build` and `cargo` have their own caches already.
- **Automatic plugin reordering**: Currently, lifecycle conflicts require the user to reorder plugins manually in `sindri.workspace`. A future version could detect and suggest (or automatically apply) a valid ordering.
- **Step opt-out**: Allow individual modules to disable specific lifecycle steps (e.g. opt out of `format` for generated modules). Declared as `disabled_steps = ["format"]` in the module's build file.
- **`sindri modules`**: List all modules in the workspace. Requires a full recursive filesystem walk from the workspace root to find all `sindri.build` files.
- **Wildcard targets** (`//libs/...`, `//...`): Build all modules under a given path or across the entire workspace. Also requires a full filesystem walk and is a natural companion to `sindri modules`.
- **Low-level compiler support for Go and Rust**: Plugins that bypass `go build`/`cargo` and invoke `go tool compile`/`rustc` directly, giving Sindri full visibility into the build graph, precise incrementality, and remote-cache eligibility at the task level. Would require Sindri to understand and translate `go.mod`/`Cargo.toml` configuration — or expose a migration path for users who want to move from native-tool mode to low-level mode.
- **Symlink support**: Allow symlinks whose targets resolve to paths within the workspace root. Requires canonicalizing paths on discovery and verifying targets during glob traversal. Symlinks pointing outside the workspace remain an error.
- **Container image building and signing**: Additional lifecycle steps for building OCI images and signing artifacts.
- **Precise Go input tracking**: The MVP tracks a platform-independent superset glob of each module's source and lets `go build` select among it (§6). Using Go's own introspection (`go list -json`) for the exact, per-target input set would additionally handle:
  - **`//go:embed`**: assets embedded into the binary via `//go:embed` directives are build inputs but are not `.go`/C source, so the superset glob does not track them — editing an embedded asset is not currently detected as a change.
  - **`//go:build` constraints**: build tags and `_GOOS_GOARCH` filename suffixes select which files compile for a given target; the superset glob deliberately ignores them (tracking all files regardless of target), trading per-target precision for platform-independent state.
