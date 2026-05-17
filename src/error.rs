use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum SindriError {
    #[error("no `sindri.workspace` file found starting from `{start}`")]
    #[diagnostic(
        help("create a `sindri.workspace` file in the root of your project"),
        code(sindri::workspace::not_found)
    )]
    WorkspaceNotFound { start: String },

    #[error("no `sindri.build` file found between `{start}` and the workspace root")]
    #[diagnostic(
        help("create a `sindri.build` file in your module directory"),
        code(sindri::module::not_found)
    )]
    ModuleNotFound { start: String },

    #[error("could not read `{path}`: {source}")]
    #[diagnostic(help("check that the file exists and is readable"), code(sindri::io))]
    Io { path: String, source: std::io::Error },

    #[error("failed to evaluate `{path}` as Nickel:\n\n{nickel_message}")]
    #[diagnostic(help("check the Nickel syntax in your build file"), code(sindri::nickel::eval))]
    NickelEval { path: String, nickel_message: String },

    #[error("`{path}` has an invalid structure: {message}")]
    #[diagnostic(
        help("check that all required fields are present and have the correct types"),
        code(sindri::schema)
    )]
    Schema { path: String, message: String },

    #[error("`{path}` is a symlink, which is not supported")]
    #[diagnostic(
        help("replace the symlink with the actual file"),
        code(sindri::symlink::not_supported)
    )]
    SymlinkNotSupported { path: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use miette::Diagnostic as _;
    use std::io;

    fn assert_has_help_and_code(error: &SindriError) {
        assert!(error.help().is_some(), "error variant is missing help text: {error:?}");
        assert!(
            error.code().is_some(),
            "error variant is missing diagnostic code: {error:?}"
        );
    }

    #[test]
    fn all_variants_have_help_and_code() {
        let io_error: io::Error = io::Error::new(io::ErrorKind::NotFound, "not found");
        assert_has_help_and_code(&SindriError::WorkspaceNotFound { start: "/tmp".into() });
        assert_has_help_and_code(&SindriError::ModuleNotFound { start: "/tmp".into() });
        assert_has_help_and_code(&SindriError::Io {
            path: "/tmp/foo".into(),
            source: io_error,
        });
        assert_has_help_and_code(&SindriError::NickelEval {
            path: "/tmp/foo".into(),
            nickel_message: "error".into(),
        });
        assert_has_help_and_code(&SindriError::Schema {
            path: "/tmp/foo".into(),
            message: "missing field".into(),
        });
        assert_has_help_and_code(&SindriError::SymlinkNotSupported {
            path: "/tmp/foo".into(),
        });
    }
}
