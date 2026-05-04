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
| `start` | Sentinel. No tasks run here. | — |
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
| `end` | Sentinel. No tasks run here. | — |

`start` and `end` exist solely as anchor points for plugins that need to run before the first or after the last built-in step.

### Source-mutating steps

`generate` and `format` are the only steps that may write to the source tree. Because `generate` precedes `format`, which precedes `compile`, any generated files are formatted and then compiled in a single build — no intermediate build is required.

Downstream steps (`compile`, `lint`, etc.) see the source tree only after both mutating steps have completed. The build tool hashes source files after the mutating steps finish and uses those hashes as the inputs for all subsequent steps.

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
# A simplified illustration of a Java compile task declaration
{
  task    = "java-compile",
  step    = "compile",
  inputs  = [ "src/main/java/**/*.java", "src/main/resources/**/*" ],
  outputs = [ ".target/classes/**/*" ],
  tools   = [ "javac" ],
  command = "javac -d .target/classes $(find src/main/java -name '*.java')",
}
```

### Input and output globs

Each task declares:

- **`inputs`**: globs (relative to the module's `sindri.build`) of files the task reads.
- **`outputs`**: globs of files the task produces.

All globs are tracked by file watchers (§9). When a file matching an input glob changes, the task is marked dirty and will re-run on the next build.

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
  command      = "codegen --input src/main/schema --output src/generated",
}
```

When `sindri` encounters a task with `module_tools`:

1. The referenced module is built up to and including its `package` step before the task runs.
2. The named binary (produced by that module's packaging) is added to the task's executable search path.
3. If the binary cannot be found after the module is built, the build fails immediately with a named error.

Workspace-built tools are therefore first-class participants in the build graph. The ordering guarantee is strict: a module that produces a tool is fully built before any task that consumes it runs, regardless of which other modules declare a dependency on that plugin.

### The `Task` schema

The build tool ships a set of typed Nickel contracts as part of its core. `sindri` applies them automatically when it loads build files and plugins — no import is required. A task declaration is simply a plain Nickel record:

```nickel
{
  task = "my-task",
  ...
}
```

`sindri` validates it against the `Task` contract at startup. If a required field is missing or has the wrong type, `sindri` reports a precise error before any build work begins.

### Native build tools vs. low-level compilers

For languages with capable native build tools — Go (`go build`) and Rust (`cargo`) being the primary examples — plugins invoke the native tool directly rather than the low-level compiler (`go tool compile`, `rustc`). This delegates dependency resolution, caching, and incremental compilation to the native tool, keeping plugins simple at the cost of reduced Sindri visibility into the build.

For languages without a dominant native build tool (Java, C, C++), plugins invoke the compiler directly and Sindri is fully in control.

In the future, a plugin may offer both modes as a user preference: native tool for simplicity, low-level compiler for full Sindri visibility, precise incrementality, and remote-cache eligibility at the task level.

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

### Incremental correctness

`sindri` maintains a persistent record (inspired by Mill) of:

- The hash of each input file at the time of the last successful task run.
- The hash of each output file at the time of the last successful task run.
- The task declaration itself (including command, tools, and glob patterns).

A task is re-run when any of the following is true:

- Any input file's current hash differs from its recorded hash.
- Any output file is missing or its hash differs from its recorded hash.
- The task declaration has changed (e.g. a plugin update changed the command).

A task is skipped when all of the above are false.

If a task fails, its hash record is not updated. On the next invocation the task will re-run: either the input hashes reflect partial changes made before the failure, or the output hashes reflect an inconsistent state. In both cases the mismatch is detected and the task re-runs automatically. No staging areas or rollback mechanism are needed — hash-based tracking makes the build self-correcting.

After a source-mutating step (`generate`, `format`) completes, `sindri` re-hashes the source tree before evaluating dirtiness for subsequent steps. This ensures that generated or reformatted files are treated as fresh inputs to the compile step.

### Caching

The build tool uses a content-addressable local cache: task outputs are stored keyed by a hash of all inputs. When a task would re-run, `sindri` first checks whether a cached output exists for the current input hash. If so, the cached outputs are restored without running the task.

The caching architecture is designed to support a remote cache backend (shared across machines and CI) as a future extension, without requiring changes to the core execution model.

### Telemetry

After every build — success or failure — `sindri` writes `<build_dir>/telemetry.json` in the **Chrome trace format**, directly loadable in [Perfetto](https://ui.perfetto.dev) or `chrome://tracing` without additional tooling.

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

By default, a module's dependencies are not visible to its consumers. If module A depends on B, a module that depends on A does not get B on its compilation classpath — it must declare B itself if it needs it.

A `compile`-scope dependency can be marked `export = true` to make it visible to consumers:

```nickel
dependencies = {
  compile = [
    { artifact = "example-org:core-lib"                  },  # not visible to consumers
    { artifact = "example-org:api-types", export = true  },  # visible to consumers
    { module   = "//libs/common",         export = true  },  # modules too
  ],
},
```

`export` is only meaningful on `compile`-scoped dependencies; it has no effect on `test`, `test-runtime`, or `runtime` scopes and is an error if set there.

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

These are resolved through the same lock file mechanism as regular dependencies. The plugin owns the distinction between compile classpath and processor path; the module just declares which processors it uses.

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
ArtifactDep   — { artifact: String, export?: Bool }
ModuleDep     — { module: String, export?: Bool }
Dependency    — ArtifactDep | ModuleDep
Dependencies  — { compile?: Array Dependency, test?: Array Dependency,
                  runtime?: Array Dependency, test-runtime?: Array Dependency }
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

2. **Plugin command execution environment**: Plugin `command` fields are strings run in the shell. Should `sindri` define a minimal portable shell subset, require POSIX sh, or support a structured command representation (list of arguments) to avoid shell quoting issues and improve portability within WSL2?

3. **Test result reporting**: How are test results reported? JUnit XML? A custom format? How does this integrate with BSP's test reporting protocol?

4. **Module version inheritance**: Can a workspace declare a default version for all modules and allow individual modules to override it?

5. **Module-level dependency version overrides**: The lock file is workspace-level, but individual modules may need to pin a different version of an artifact (e.g. a binary module depending on an older library than the rest of the workspace). The mechanism for declaring module-level overrides and the edge cases they introduce (two modules pinning different versions of the same artifact while a third depends on both) need careful design.

---

## 16. Future Work

Items agreed to be out of scope for the initial implementation.

- **Verbose / investigative mode**: A mode (e.g. `sindri --investigate`) that logs which files triggered a rebuild, which tasks were marked dirty and why, and what the file watcher observed. Useful for diagnosing misconfigured globs or unexpected rebuild loops.
- **Local and remote caching**: Store task outputs in a content-addressable cache (keyed by input hash) so that reverting a change restores outputs without re-running the task. The same structure supports a remote cache backend shared across machines and CI. Most valuable once Sindri invokes low-level compilers directly — native tools like `go build` and `cargo` have their own caches already.
- **Automatic plugin reordering**: Currently, lifecycle conflicts require the user to reorder plugins manually in `sindri.workspace`. A future version could detect and suggest (or automatically apply) a valid ordering.
- **Step opt-out**: Allow individual modules to disable specific lifecycle steps (e.g. opt out of `format` for generated modules). Declared as `disabled_steps = ["format"]` in the module's build file.
- **`sindri modules`**: List all modules in the workspace. Requires a full recursive filesystem walk from the workspace root to find all `sindri.build` files.
- **Wildcard targets** (`//libs/...`, `//...`): Build all modules under a given path or across the entire workspace. Also requires a full filesystem walk and is a natural companion to `sindri modules`.
- **Low-level compiler support for Go and Rust**: Plugins that bypass `go build`/`cargo` and invoke `go tool compile`/`rustc` directly, giving Sindri full visibility into the build graph, precise incrementality, and remote-cache eligibility at the task level. Would require Sindri to understand and translate `go.mod`/`Cargo.toml` configuration — or expose a migration path for users who want to move from native-tool mode to low-level mode.
- **Symlink support**: Allow symlinks whose targets resolve to paths within the workspace root. Requires canonicalizing paths on discovery and verifying targets during glob traversal. Symlinks pointing outside the workspace remain an error.
- **Container image building and signing**: Additional lifecycle steps for building OCI images and signing artifacts.
