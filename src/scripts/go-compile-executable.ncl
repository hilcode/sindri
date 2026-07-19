# Compile every package in the module and link a runnable binary into the tracked output
# directory, so the binary itself becomes a tracked output.
fun inputs =>
  let output_directory = inputs."output-directory" in
  [ { program = "go", arguments = [ "build", "-o", "%{output_directory}/", "./..." ] } ]
