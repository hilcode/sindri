{
  name | String,      # module name (required)
  language | String,  # one of: go, rust, java, kotlin, zig (required)
  type | String,      # one of: library, executable, web-archive, container-image (required)
  version | String,   # module version, e.g. "0.1.0" (required)
}
