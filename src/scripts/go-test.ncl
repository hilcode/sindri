# Run every test in the module. GOWORK points `go` at the generated workspace file, so imports of
# sibling modules resolve to their local directories.
fun inputs =>
  let go_work = "%{inputs."managed-input-directory"}/go.work" in
  [ { program = "go", arguments = [ "test", "./..." ], environment = { GOWORK = go_work } } ]
