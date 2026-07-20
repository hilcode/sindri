# Compile every package in the module and link a runnable binary into the tracked output
# directory, so the binary itself becomes a tracked output. GOWORK points `go` at the generated
# workspace file, so imports of sibling modules resolve to their local directories.
fun inputs =>
  let output_directory = inputs."output-directory" in
  let go_work = "%{inputs."managed-input-directory"}/go.work" in
  [ { program = "go", arguments = [ "build", "-o", "%{output_directory}/", "./..." ], environment = { GOWORK = go_work } } ]
