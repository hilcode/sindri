# Lifecycle Plan — lifecycles as data

Grew out of a small request ("expose `format`/`test`/`package` as CLI
subcommands alongside `compile`") that turned out to be a symptom of a
bigger issue: `Lifecycle::new()` hardcodes the one and only lifecycle in
Rust, and `clean` runs through a completely separate, bespoke path
(`Lifecycle::run_clean`) that bypasses the `steps`/`build_task_graph`
machinery every other step goes through. This plan moves lifecycles onto
disk instead of adding another round of CLI-only changes that would need
undoing once real redefinition/downloading lands.

Companion to `PLAN.md`; kept separate because it's an orthogonal initiative
(the CLI/lifecycle surface) rather than the next sequential phase of the
MVP checklist.

**Phases 1 (`.sindri/lifecycles/` as data, bootstrap + checksum
verification) and 2 (dynamic CLI subcommands sourced from loaded
lifecycles) are complete and merged.** Full detail is in git/jj history,
not repeated here.

---

## Phase 3 — `.sindri/plugins/`

Goal: `GoPlugin` stops being the one hardcoded Rust plugin. Sindri reads
the Go plugin's tasks (scripts, glob patterns, parameters) from
`.sindri/plugins/go/`, sourced the same way `.sindri/lifecycles/` already
is: canonical content embedded in the binary, auto-seeded to
`.sindri/plugins/` when missing, checksum-verified on every load, a
mismatch a hard startup error. **The mechanism is unchanged from Phase
1e — same as lifecycles, deliberately.** Content stays embedded in the
binary for now; a plugin's *origin* can later change from "embedded" to
"fetched from somewhere" (DESIGN.md §15's open "plugin distribution"
question) without touching the lookup/checksum path itself. What actually
changes is that `GoPlugin` stops being hardcoded Rust *logic* (a struct
whose method manually constructs `Task` values in code) and becomes inert
embedded *data* instead — the same shift Phase 1e already made for
lifecycles (`Lifecycle::default()`'s hardcoded 10-step Rust list →
`src/lifecycles/default.json`).

The main structural difference from Phase 1e: a lifecycle is one flat JSON
file, but a plugin is a *manifest plus several script files* — the
embed/seed/checksum machinery generalizes from "one file per known thing"
to "a small fixed set of files per known plugin."

`GoPlugin` (`src/go_plugin.rs`) today: a zero-sized marker type with no
trait, plain inherent functions called directly and unconditionally from
`src/lifecycle.rs` (four call sites) for every module, regardless of
`Language` (a single-variant enum, `Go` only — no plugin registry exists to
generalize prematurely, and this phase doesn't invent one).
`GoPlugin::tasks(artifact_type)` returns three hardcoded `(Task, Step)`
pairs (`go-format`/`format`, `go-compile`/`compile`, `go-test`/`test`);
`compile_parameters()` declares the `mode` parameter. `Task`
(`src/task.rs`) is `name`, `script: Script`, `declared_input`,
`managed_input`, `output` (three `FileSetPattern` wrappers),
`declared_parameters: ParameterDeclarations`, `module_tools:
Vec<ModuleToolReference>`. `Script` (`src/script.rs`) is just a Nickel
expression string; today's named constructors embed shipped `.ncl` files
via `include_str!` (`go_format()`, `go_compile()`,
`go_compile_executable()`, `go_test()`) — these become the embedded source
for the plugin's script files. `ParameterDeclarations`
(`src/parameter.rs`) is already `BTreeMap<(PluginName, ParameterName),
ParameterType>` — `PluginName` already exists as a newtype, keyed exactly
the way a multi-plugin world needs.

### 3a — Plugin type surface

- [x] A `Plugin` struct (name: reuse the existing `PluginName`; tasks bound
  to steps, mirroring what `Lifecycle` is to `Step`). Added in
  `src/plugin.rs`: `name: PluginName`, `tasks: Vec<(Task, Step)>`, with
  `name()`/`tasks()` accessors — construction and loading are out of scope
  here, added in 3b.
- [ ] Decide the manifest format — JSON shape for task/step/glob/parameter
  structure, left open here — with script bodies as separate `.ncl` files
  referenced by relative path within the plugin directory. A real on-disk
  plugin file's own path becomes its `script_path` directly; no synthetic-
  location machinery is needed the way embedded/inline sources need one
  today (`Task::script_path`, `task.rs`). Deferred to 3b, alongside
  `Plugins::load` itself, which is what actually needs a concrete shape to
  parse into.

### 3b — `.sindri/plugins/` bootstrap + checksum-verified load

- [x] New `src/plugins.rs`, generalizing `KNOWN_LIFECYCLES`/
  `Lifecycles::load`'s exact shape from "one file per known thing" to "a
  small fixed set of files per known plugin" (a manifest plus each of its
  script files). The manifest format decided here: a JSON `{ "tasks": [...] }`
  list, each entry naming a task/step/script-relative-path plus glob lists
  for declared/managed input and output and a parameter list (each entry's
  own `plugin`/`name`/`type`) — mirrors `Task`/`Step` field-for-field, parsed
  by `TaskManifest`/`ParameterManifest`. A checksum manifest key is a
  `RelativeFile` relative to `.sindri/plugins/` itself (e.g. `go/manifest.json`),
  since — unlike lifecycles' flat single-file-per-thing layout — a plugin's
  files are nested under its own subdirectory and share one manifest with
  every other known plugin.
- [x] `PluginRegistry::load(workspace, file_system) -> SindriResult<PluginRegistry>`:
  locate/create `.sindri/plugins/`; for each known plugin, seed any missing
  file (and record its checksum) from embedded content — per-file, so a
  partially present plugin only has its missing pieces recreated, same as
  `Lifecycles::load` does per lifecycle file today; checksum-verify every
  file (freshly seeded or pre-existing) against the manifest; mismatch → a
  new `SindriError::PluginModified` (mirrors `LifecycleModified`, "editing
  plugin files isn't supported yet" help text), a hard error before any
  build work starts; parse into `Plugin`/`Task` structures. Split into
  `load_known_file` (one file's seed-or-read-then-verify) and
  `load_known_plugin` (one plugin's files plus its manifest parse) rather
  than one large function, so each piece of branching is independently
  readable. Named `PluginRegistry`, not `Plugins` — a specific, singular
  collection (every plugin this binary knows about), not a bare plural.
  - [x] Test: seeds missing files (and a checksum manifest) when absent.
  - [x] Test: recreates only the missing piece of a partially-present
    plugin, leaving valid siblings untouched.
  - [x] Test: reads a pre-seeded, checksum-matching plugin without
    rewriting anything.
  - [x] Test: a mismatched file fails with `PluginModified` before any
    build work starts.
- [x] Route `.sindri/plugins/`'s path through `Workspace::
  absolute_sindri_directory()` — fixing an existing seam where
  `Lifecycles::load` builds `.sindri/lifecycles/`'s path inline instead of
  using that shared accessor — rather than duplicating the seam here.
  `PluginRegistry::load` takes `&Workspace` (not `&WorkspaceRoot`, unlike
  `Lifecycles::load`) so it can reach that accessor directly.
- [x] `PluginRegistry`'s no-filesystem twin: not built. Nothing calls
  `PluginRegistry::load` yet (that starts in 3c), so there is no real
  caller to justify one ahead of time.
- [x] The shared checksum-manifest machinery itself (seed/verify/read
  content, keyed and valued by proper domain types — a `RelativeFile` key,
  a `Checksum` newtype wrapping a real `blake3::Hash` rather than a bare
  hex `String`) moved out of `lifecycles.rs` into `src/checksums.rs`, so
  `PluginRegistry::load` and `Lifecycles::load` share one implementation
  instead of two copies of the same seed/checksum-verify logic.
- [x] The Go plugin's executable-linking script — bound to `compile` today,
  selected by an `if artifact_type == Executable` branch inside
  `GoPlugin::tasks()` — moves to a `package`-step task instead
  (`go-package`, using the renamed `scripts/go-package.ncl`, formerly
  `go-compile-executable.ncl`). This closes an existing gap noted in
  `lifecycle.rs`'s tool-target build path ("nothing is bound to `package`
  itself for Go today — the binary is already a tracked `compile` output")
  and turns `go-compile` into one universal task with no per-artifact-type
  branching at all (`go build ./...`, identical for every module); only
  `go-package` is executable-only, a smaller conditional-application
  problem for 3c than picking between two full `go-compile` variants.
  3c must update `lifecycle.rs`'s tool-target binary lookup (currently
  `TaskName::new("go-compile")`) to resolve the binary from `go-package`'s
  output instead.

### 3c — Rewire `GoPlugin` callers

- [x] Delete `GoPlugin::tasks()` / `compile_parameters()`; the `lifecycle.rs`
  call sites resolve tasks through the loaded `PluginRegistry` instead, via
  a new `GoPlugin::tasks_for(plugins, artifact_type)`.
- [x] Apply `go-package` only to executable modules (library modules get
  `go-format`/`go-compile`/`go-test` only) — `tasks_for` filters it out
  with `ArtifactType::is_executable()`.
- [x] Update the tool-target binary lookup in `lifecycle.rs` (was
  `TaskName::new("go-compile")`) to resolve against `go-package`'s output
  directory instead, since that's the task that now actually links and
  places the tracked binary.
- [x] **Finding, resolved with the user:** binding the executable-linking
  task to `package` (3b's design) means `sindri compile` alone no longer
  produces a binary for an ordinary executable module — `build_task_graph`
  only runs steps up to and including the invocation's own target, and
  `compile` never reaches `package`. This is a real, intentional behavior
  change, not a bug to route around: `compile` type-checks; `package`
  (which already runs everything `compile` does, plus `test`, plus
  `go-package`) is what actually links and places the binary — matching
  the Maven/Gradle-style phase distinction the lifecycle's own step order
  (`… → test → integration-test → package → publish`) already implies.
  Confirmed by updating every `tests/cli.rs` case that reads a compiled
  binary to invoke `package` instead of `compile`; cases that only check
  that `go-compile` itself ran (not that a binary exists) were left
  invoking `compile`, unchanged.
- [x] `generate_go_work_task` stays Rust-side glue *for this phase only* —
  its script content is built via `format!()` embedding the discovered
  list of Go module directories, data no task's `inputs` can express yet.
  Phase 4 below eliminates this gap.
- [x] `module_tool::invocation_task` is a different case and stays
  Rust-side permanently: it's not Go-specific at all — `module_tools` is a
  cross-cutting Sindri mechanism any module can use regardless of
  language, and its script is already a trivial static one-liner — so it
  was never a candidate for `.sindri/plugins/go/` in the first place.
- [x] Preserve today's "one known plugin, always applied to every module"
  behavior rather than inventing a language-to-plugin dispatch mechanism
  ahead of a second real language: `tasks_for` flattens every loaded
  plugin's tasks unconditionally (today, just `go`'s), the same way
  `GoPlugin::tasks()` applied unconditionally before.

### 3d — Validation invariants

- [ ] Mirror `validate_no_step_collisions` — no two plugins bind a task to
  a colliding name.
  - [ ] Test: two plugins binding the same task name → a named startup
    error.
- [ ] Confirm, with an explicit test rather than an assumption, that
  `ParameterDeclarations`'s existing `(PluginName, ParameterName)` keying
  already keeps multi-plugin parameters disjoint.

---

## Phase 4 — Workspace-discovered inputs

Goal: eliminate the last piece of Rust-hardcoded script generation in
`GoPlugin`. `generate_go_work_task`'s Nickel source is currently built via
`format!()`, embedding the discovered list of Go module directories
directly into generated text — the one task whose script isn't static
content, and so couldn't move to `.sindri/plugins/go/` in Phase 3. Flagged
by the user as needing to be addressed ASAP, right after Phase 3 lands —
not left as an indefinitely-deferred footnote.

**Problem**: a task script's `inputs` today only carries two kinds of
resolved data — glob-matched files, and (since Phase 5g of `PLAN.md`) a
`module_tools` binary's resolved absolute path via `inputs."module-tools"
.<binary>`, a hermetic data lookup computed by Sindri before the script
evaluates. Neither can express "the directory of every Go module in this
workspace," discovered only after `ModuleGraph::load` runs and not scoped
to any single module's globs.

**Direction (committed, not open)**: generalize the `module_tools`
precedent — a second kind of computed, non-glob input resolved by Sindri
and injected into the same `inputs` record before evaluation — rather than
inventing an unrelated mechanism. Concretely, a workspace-scoped discovered
input (e.g. `inputs."go-work"."module-directories"`, a list of
workspace-relative paths) computed from the loaded `ModuleGraph`. Once it
exists, `generate_go_work_task` becomes a fixed, embedded `.ncl` file under
`.sindri/plugins/go/scripts/go-work.ncl` — data, seeded and
checksum-verified exactly like every other Go script from Phase 3 — that
reads the injected module-directory list and formats `go.work`'s content in
Nickel. No Rust-side string templating left anywhere in `GoPlugin`.

Exact subphase breakdown (the injection point in `lifecycle.rs`/`task.rs`,
the new input's precise key shape, its test list) is deferred to when this
phase starts — the destination is settled, the mechanics aren't yet.

**Explicitly not in scope here**: `module_tool::invocation_task`. It's
unrelated — not Go-specific, and its script is already a trivial static
one-liner (`fun inputs => [ { program = inputs."module-tools"."<binary>" }
]`) — so it was never blocked on this and needs no change.

---

## Explicitly out of scope (this plan)

- Actual support for hand-editing or authoring lifecycle/plugin files —
  Phase 1 and Phase 3 both enforce the *opposite* (checksum-rejects drift)
  as an honest placeholder until redefinition has a real design.
- Actual plugin fetching/downloading — stays a `DESIGN.md` §15 open
  question (plugin versioning/distribution).
- Arbitrary/third lifecycle names beyond `default`/`clean`, or a second
  real plugin/language beyond `go` — the known-name lists are fixed in
  this version.
- Plugin-driven lifecycle step insertion (`pre`/`post`, DESIGN.md §5).
- Reconciling `.sindri/plugins/` with `PLAN.md` Phase 8's separate
  workspace-root `plugins/` directory (user-authored custom tasks, checked
  into the workspace, never fetched/distributed) — a different directory
  with a different purpose. Deferred until after this plan is fully
  implemented, per the user; `PLAN.md`/`DESIGN.md` get updated then, not
  now.
- Module targeting (`sindri <step> <target>`), `watch`, `deps`, `plugin
  validate/new`, `bsp` — untouched.

## Verification

- `just lint`, `just format`, `just test`.
- `sindri compile` in a fixture Go module, sourcing its tasks from
  `.sindri/plugins/go/` instead of the hardcoded struct.
- Delete a workspace's `.sindri/plugins/` directory and re-run `sindri
  compile` — confirms it's silently recreated with the same defaults and a
  fresh checksum manifest, mirroring lifecycles.
- Hand-edit a seeded plugin file and re-run any command — confirms it now
  fails fast with `PluginModified` rather than silently taking effect, the
  same "hard error, not silently honored" behavior `LifecycleModified`
  already has for lifecycles.
