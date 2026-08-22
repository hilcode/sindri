# Compile every package in the module and link a runnable binary into the tracked output
# directory, so the binary itself becomes a tracked output. GOWORK points `go` at the generated
# workspace file, so imports of sibling modules resolve to their local directories. `params."sindri-go".mode`
# selects a release build (`-trimpath -ldflags "-s -w"`, stripping symbols and embedded build paths)
# over the Go toolchain's own debug-build defaults.
fun inputs =>
  let output_directory = inputs."output-directory" in
  let go_work = "%{inputs."managed-input-directory"}/go.work" in
  let mode_arguments = if inputs.params."sindri-go".mode == "release" then [ "-trimpath", "-ldflags", "-s -w" ] else [] in
  [ { program = "go", arguments = [ "build" ] @ mode_arguments @ [ "-o", "%{output_directory}/", "./..." ], environment = { GOWORK = go_work } } ]
