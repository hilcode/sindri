# Examples

Small, self-contained projects that exercise Sindri's current functionality. Each
directory is a single-module workspace: a `sindri.workspace` marking the workspace
root and a `sindri.build` describing the module, alongside the module's own sources.

Multi-module workspaces and inter-module dependencies are not implemented yet
(see `PLAN.md`, Phase 5), so every example keeps its `sindri.workspace` and
`sindri.build` in the same directory.

## The examples

| Directory  | What it shows                                                              |
| ---------- | -------------------------------------------------------------------------- |
| `hello/`   | The smallest possible Go executable module — one `main.go`.                |
| `greeter/` | A Go executable split across a `main` package and a `greeting/` subpackage, so incremental rebuilds after editing a single file are observable. |

## Running them

Install the `sindri` binary once from the repository root, then drive the examples
with the `justfile` in this directory:

```sh
# From the repository root — builds and installs `sindri` onto your PATH.
just install            # or: just install release

cd examples
just compile-all        # compile every example
just compile hello      # compile just one
just lifecycle hello    # print its resolved lifecycle steps and tasks
```

`compile` runs the `format` step (`gofmt`) and then the `compile` step
(`go build`). The built binary lands under
`.target/default/compile/go-compile/output/`.

The recipes call `sindri` from your `PATH`, so `just install` must have run first.
To invoke the tool directly instead, run `sindri compile` from inside an example
directory.

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

`just clean-all` removes every example's build output (`just clean <example>` for
one). The `.target/` build directory each run produces is git-ignored.
