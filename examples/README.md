# Examples

Small, self-contained projects that exercise Sindri's current functionality. Each
example is a workspace rooted at a `sindri.workspace` file, with one or more modules
described by `sindri.build` files alongside their own sources.

## The examples

| Directory       | What it shows                                                         |
| --------------- | -------------------------------------------------------------------- |
| `hello/`        | The smallest possible Go executable module — one `main.go`.          |
| `greeter/`      | A Go executable split across a `main` package and a `greeting/` subpackage, so incremental rebuilds after editing a single file are observable. |
| `multi-module/` | An executable module that declares a `{ module = … }` dependency on a local Go library module in a `lib/greeting/` subdirectory — two modules built in dependency-first order. |

## Running them

Drive the examples with the `justfile` in this directory:

```sh
cd examples
just compile-all        # compile every example
just compile hello      # compile just one
just lifecycle hello    # print its resolved lifecycle steps and tasks
```

Each recipe first rebuilds and reinstalls `sindri` if the sources changed (the
`install` dependency defers to `cargo`, so it is a no-op when nothing changed), then
calls `sindri` from your `PATH`. You never have to remember to reinstall after editing
the tool — including its embedded `*.ncl` contracts.

`compile` runs the `format` step (`gofmt`) and then the `compile` step
(`go build`). The built binary lands under
`.target/go-compile/<binding-hash>/` — the hash names this particular build's
parameter binding, so different bindings of the same task can one day coexist
side by side without overwriting each other.

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
just compile multi-module           # builds lib/greeting, then app
cat multi-module/.target/go.work    # generated: a `use` entry per module
```

> **Known gap:** `go.work` is generated, but nothing points `go` at it via `GOWORK`
> yet, so `just compile multi-module` currently fails to resolve the sibling import.
> Wiring the generated file back into the build as a tracked input is in progress.

`just clean-all` removes every example's build output (`just clean <example>` for
one). The `.target/` build directory each run produces — including the generated
`go.work` — is git-ignored.
