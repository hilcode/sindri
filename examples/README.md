# Examples

Small, self-contained projects that exercise Sindri's current functionality. Each
example is a workspace rooted at a `sindri.workspace` file, with one or more modules
described by `sindri.build` files alongside their own sources.

## The examples

| Directory         | What it shows                                                         |
| ----------------- | -------------------------------------------------------------------- |
| `hello/`          | The smallest possible Go executable module — one `main.go`.          |
| `greeter/`        | A Go executable split across a `main` package and a `greeting/` subpackage, so incremental rebuilds after editing a single file are observable. |
| `multi-module/`   | An executable module that declares a `{ module = … }` dependency on a local Go library module in a `lib/greeting/` subdirectory — two modules built in dependency-first order. |
| `parameterized/`  | A Go executable whose `sindri.build` binds `sindri-go.mode`, showing how two parameter bindings of the same module coexist in the build directory. |
| `codegen/`        | An executable module that declares `module_tools`, naming a small code generator built by Sindri from a local `tools/codegen/` module and run during `generate` — its output is compiled in the same build. |

## Running them

Drive the examples with the `justfile` in this directory:

```sh
cd examples
just compile-all        # compile every example
just compile hello      # compile just one
just lifecycle hello    # print its resolved lifecycle steps and tasks
just verify-all         # build every example and check it actually does what it claims to
just verify multi-module
```

Each recipe first rebuilds and reinstalls `sindri` if the sources changed (the
`install` dependency defers to `cargo`, so it is a no-op when nothing changed), then
calls `sindri` from your `PATH`. You never have to remember to reinstall after editing
the tool — including its embedded `*.ncl` contracts.

`verify` runs an example's own `verify.sh` (see that file for what it checks).

`compile` runs the `format` step (`gofmt`) and then the `compile` step
(`go build`). The built binary lands under
`.target/go-compile/<binding-hash>/` — the hash names this particular build's
parameter binding, so different bindings of the same task coexist side by side
without overwriting each other (see "Parameter bindings" below).

### Incremental correctness

Run `compile` a second time without changing anything — Sindri hashes each task's
inputs and outputs, sees they are unchanged, and skips the work, printing nothing:

```sh
just compile hello      # builds
just compile hello      # silent — nothing to do
```

Edit a source file (for example `greeter/greeting/greeting.go`) and compile again to
see just the affected task re-run. Deleting the built binary and recompiling
triggers a rebuild too — outputs are tracked, not just inputs.

### Multi-module builds

`multi-module/` shows an executable that depends on a local library module. Its
`sindri.build` declares:

```nickel
dependencies = { compile = [ { module = "//lib/greeting" } ] }
```

`sindri compile` loads the whole reachable module graph and builds the library before
the executable that consumes it. Because `go build` only resolves an import of a
sibling module when a `go.work` lists both module directories, Sindri generates that
`go.work` — from the declared dependencies, so it never drifts from a hand-maintained
file — and points `go` at it via the `GOWORK` environment variable before the compile
runs. The file lives inside the build directory, so it is a pure build artifact that
never touches the source tree:

```
just compile multi-module                              # builds lib/greeting, then app
cat multi-module/.target/generate-go-work/*/go.work     # generated: a `use` entry per module
```

### Workspace-built tools

`codegen/` shows a task consuming a binary built by another module in the same
workspace. Its `sindri.build` declares:

```nickel
module_tools = [ "//tools/codegen:codegen" ]
```

— a label naming a module identity and, after the `:`, the binary that module's
`package` step must produce. Before `app`'s own tasks run, Sindri builds
`//tools/codegen` up to and including `package` (for Go, its `compile` step already
produces the binary — DESIGN's `package` step has nothing further to do), then
synthesizes a `generate`-step task that runs the resolved binary directly — the
absolute path is handed to it, not looked up on `PATH` — with its working directory
set to `app`'s own module directory:

```sh
just compile codegen                    # builds tools/codegen, then app
cat codegen/generated_greeting.go       # written by the codegen binary during `generate`
codegen/.target/go-compile/*/app        # the compiled binary, calling the generated function
```

Because `generate` precedes `compile` in the lifecycle, the generated file is already
on disk by the time `app`'s own `go-compile` runs — no intermediate build is needed,
and no explicit input glob has to mention it. Editing `tools/codegen/main.go` and
recompiling regenerates the file and rebuilds `app`, since a rebuilt tool module
propagates to every module that names it in `module_tools`, the same way a rebuilt
`{ module = … }` dependency does.

### Parameter bindings

`parameterized/`'s `sindri.build` binds a value for `go-compile`'s `mode` parameter:

```nickel
parameters = { "sindri-go" = { mode = "debug" } }
```

`mode` selects between a debug build (the Go toolchain's own defaults) and a release
build (`-trimpath -ldflags "-s -w"`, stripping symbols and embedded paths). Every
module built with `go-compile` must set it — there is no implicit default.

Edit the value and rebuild to see the two bindings coexist rather than overwrite one
another:

```sh
just compile parameterized                          # builds with mode = "debug"
sed -i 's/mode = "debug"/mode = "release"/' parameterized/sindri.build
just compile parameterized                          # builds again with mode = "release"
ls parameterized/.target/go-compile/                 # two binding-hash directories, side by side
```

The debug binary is larger and unstripped; the release binary is smaller and stripped
(`file parameterized/.target/go-compile/*/parameterized` shows the difference). Both
directories persist until `just clean parameterized` removes the whole `.target/`.

`just clean-all` removes every example's build output (`just clean <example>` for
one). The `.target/` build directory each run produces — including the generated
`go.work` — is git-ignored.
