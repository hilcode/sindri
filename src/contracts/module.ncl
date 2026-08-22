# A dependency names exactly one of a workspace module (`module = "//path"`) or an external artifact
# (`artifact = "group:name"`). The closed records make "both" and "neither" contract errors, and leave
# no room for a stray field such as `export`.
let Dependency = std.contract.any_of [
  { module | String },
  { artifact | String },
] in
{
  name | String,      # module name (required)
  language | String,  # one of: go (required)
  type | String,      # one of: library, executable, web-archive, container-image (required)
  version | String,   # module version, e.g. "0.1.0" (required)
  dependencies
    | {
      # `compile` and `export` are both on the compilation path; `export` additionally re-exports its
      # entries to this module's consumers.
      compile | Array Dependency | optional,
      export | Array Dependency | optional,
      test | Array Dependency | optional,
      runtime | Array Dependency | optional,
      "test-runtime" | Array Dependency | optional,
    }
    | optional,
  # Parameter values, keyed by owning plugin then parameter name — e.g. `{ sindri-go = { mode =
  # "release" } }`. A task's declared parameters (owned by the plugin that defines the task) are
  # validated against these before its script ever runs.
  parameters | { _ : { _ : String } } | optional,
  # Workspace-built tools this module's tasks consume, e.g. `"//tools/codegen:codegen"` — a module
  # identity, `:`, then the binary that module's package step must produce. Parsed and validated on
  # the Rust side, the same way a `dependencies` module identity is.
  module_tools | Array String | optional,
}
