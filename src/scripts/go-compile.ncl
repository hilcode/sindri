# Compile every package in the module, type-checking the whole tree. GOWORK points `go` at the
# generated workspace file, so imports of sibling modules resolve to their local directories.
fun inputs =>
  let go_work = "%{inputs."managed-input-directory"}/go.work" in
  [ { program = "go", arguments = [ "build", "./..." ], environment = { GOWORK = go_work } } ]
