{
  name | String,                        # workspace name (required)
  sindri_version | String,              # required Sindri version, e.g. "0.1.0"
  build_directory | RelativeDirectory | optional,  # build output directory (default: .target)
  plugins | Array {
    name | String,
    version | String,
  } | optional,
}
