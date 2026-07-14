# The contract every command a task script produces must satisfy. `program` is the executable and is
# required; the other fields have defaults, so a script need only name the program it runs. The
# `working-directory` is relative to the workspace root and defaults to the module's own directory,
# which Sindri supplies when it applies this contract.
fun default_working_directory => {
  program | String,
  arguments | Array String | default = [],
  environment | { _ : String } | default = {},
  "working-directory" | String | default = default_working_directory,
}
