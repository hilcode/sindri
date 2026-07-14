# Compile every package in the module, type-checking the whole tree.
fun inputs => [ { program = "go", arguments = [ "build", "./..." ] } ]
