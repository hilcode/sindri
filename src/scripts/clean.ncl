# Remove every top-level entry directly under the build directory except this task's own state
# (excluded from `inputs.files` by the task's managed input pattern before this script ever runs —
# see `Lifecycle::run_clean`). `inputs.files` lists individual files, workspace-relative, so this
# derives each file's top-level directory name under the build directory (stripping the
# workspace-relative build-directory prefix, computed here rather than passed in, so this script
# stays a plain static file like every other shipped script) and deduplicates before removing them —
# a single `rm -rf go-compile lib telemetry.json ...` rather than one command per file, which would
# leave now-empty directories behind.
fun inputs =>
  let build_directory_relative =
    std.string.substring
      (std.string.length inputs."workspace-root")
      (std.string.length inputs."managed-input-directory")
      inputs."managed-input-directory"
  in
  let strip_build_directory = fun file =>
    std.string.substring (std.string.length build_directory_relative) (std.string.length file) file
  in
  let top_level_directory = fun file => std.array.first (std.string.split "/" (strip_build_directory file)) in
  let directories = std.array.dedup (std.array.map top_level_directory inputs.files) in
  [ { program = "rm", arguments = [ "-rf" ] @ directories, "working-directory" = build_directory_relative } ]
