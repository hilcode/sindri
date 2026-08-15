use crate::module::BinaryName;
use crate::parameter::ParameterName;
use crate::parameter::PluginName;
use crate::task::TaskName;
use crate::types::ModuleCycle;
use crate::types::ModuleIdentity;
use miette::Diagnostic;
use std::io::Error as IoError;
use std::path::PathBuf;
use thiserror::Error;

pub type SindriResult<T> = Result<T, SindriError>;

#[derive(Debug, Error, Diagnostic)]
pub enum SindriError {
    #[error("no `sindri.workspace` file found starting from `{}`", start.display())]
    #[diagnostic(
        help("create a `sindri.workspace` file in the root of your project"),
        code(sindri::workspace::not_found)
    )]
    WorkspaceNotFound { start: PathBuf },

    #[error("no `sindri.build` file found between `{}` and the workspace root", start.display())]
    #[diagnostic(
        help("create a `sindri.build` file in your module directory"),
        code(sindri::module::not_found)
    )]
    ModuleNotFound { start: PathBuf },

    #[error("could not read `{}`: {source}", path.display())]
    #[diagnostic(help("check that the file exists and is readable"), code(sindri::io))]
    Io { path: PathBuf, source: IoError },

    #[error("could not write to the log: {source}")]
    #[diagnostic(
        help("check that the build directory is writable and the disk is not full"),
        code(sindri::log)
    )]
    Log { source: IoError },

    #[error("failed to evaluate `{}` as Nickel:\n\n{nickel_message}", path.display())]
    #[diagnostic(help("check the Nickel syntax in your build file"), code(sindri::nickel::eval))]
    NickelEval { path: PathBuf, nickel_message: String },

    #[error("`{}` has an invalid structure: {message}", path.display())]
    #[diagnostic(
        help("check that all required fields are present and have the correct types"),
        code(sindri::schema)
    )]
    Schema { path: PathBuf, message: String },

    #[error("`{}` is a symlink, which is not supported", path.display())]
    #[diagnostic(
        help("replace the symlink with the actual file"),
        code(sindri::symlink::not_supported)
    )]
    SymlinkNotSupported { path: PathBuf },

    #[error("dependency cycle detected: {cycle}")]
    #[diagnostic(
        help("break the cycle by removing one of the `module` dependencies along it"),
        code(sindri::module::cycle)
    )]
    DependencyCycle { cycle: ModuleCycle },

    #[error(
        "module `{dependency}` is a {kind}, but only library modules may be a dependency (required by `{dependent}`)"
    )]
    #[diagnostic(
        help("make the dependency a library module, or remove the dependency"),
        code(sindri::module::non_library_dependency)
    )]
    NonLibraryDependency {
        dependent: ModuleIdentity,
        dependency: ModuleIdentity,
        kind: &'static str,
    },

    #[error(
        "module `{dependency}` is a {kind}, but only executable modules may be used as a module tool (required by `{dependent}`)"
    )]
    #[diagnostic(
        help("make the referenced module an executable module, or remove the module_tools entry"),
        code(sindri::module::non_executable_module_tool)
    )]
    NonExecutableModuleTool {
        dependent: ModuleIdentity,
        dependency: ModuleIdentity,
        kind: &'static str,
    },

    #[error("module `{module}`'s package step did not produce a binary named `{binary}`")]
    #[diagnostic(
        help(
            "check that the module's package step writes a file named {binary} into its output, or correct the binary name in module_tools"
        ),
        code(sindri::module_tool::binary_missing)
    )]
    ModuleToolBinaryMissing { module: ModuleIdentity, binary: BinaryName },

    #[error("task `{task_name}` failed\n\ncommand: {command}\n\noutput:\n{output}")]
    #[diagnostic(help("check the command output above for details"), code(sindri::task::failed))]
    TaskFailed {
        task_name: TaskName,
        command: String,
        output: String,
    },

    #[error("could not evaluate the task script:\n\n{nickel_message}")]
    #[diagnostic(
        help("a script must evaluate to a list of command records, each with at least a `program`"),
        code(sindri::script::eval)
    )]
    ScriptEvaluation { nickel_message: String },

    #[error("missing a value for parameter `{plugin}.{parameter}`")]
    #[diagnostic(
        help("supply a value for `{plugin}.{parameter}` in the module or build configuration"),
        code(sindri::parameter::missing)
    )]
    ParameterMissing {
        plugin: PluginName,
        parameter: ParameterName,
    },

    #[error("value for parameter `{plugin}.{parameter}` does not satisfy its declared type:\n\n{nickel_message}")]
    #[diagnostic(
        help("change the value so it satisfies `{plugin}.{parameter}`'s declared type"),
        code(sindri::parameter::invalid)
    )]
    ParameterInvalid {
        plugin: PluginName,
        parameter: ParameterName,
        nickel_message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::ErrorKind;

    fn assert_has_help_and_code(error: &SindriError) {
        assert!(error.help().is_some(), "error variant is missing help text: {error:?}");
        assert!(
            error.code().is_some(),
            "error variant is missing diagnostic code: {error:?}"
        );
    }

    #[test]
    fn all_variants_have_help_and_code() {
        let io_error: IoError = IoError::new(ErrorKind::NotFound, "not found");
        assert_has_help_and_code(&SindriError::WorkspaceNotFound {
            start: PathBuf::from("/tmp"),
        });
        assert_has_help_and_code(&SindriError::ModuleNotFound {
            start: PathBuf::from("/tmp"),
        });
        assert_has_help_and_code(&SindriError::Io {
            path: PathBuf::from("/tmp/foo"),
            source: io_error,
        });
        assert_has_help_and_code(&SindriError::Log {
            source: IoError::new(ErrorKind::WriteZero, "disk full"),
        });
        assert_has_help_and_code(&SindriError::NickelEval {
            path: PathBuf::from("/tmp/foo"),
            nickel_message: "error".into(),
        });
        assert_has_help_and_code(&SindriError::Schema {
            path: PathBuf::from("/tmp/foo"),
            message: "missing field".into(),
        });
        assert_has_help_and_code(&SindriError::SymlinkNotSupported {
            path: PathBuf::from("/tmp/foo"),
        });
        assert_has_help_and_code(&SindriError::DependencyCycle {
            cycle: ModuleCycle::new(vec![
                ModuleIdentity::parse("//a").unwrap(),
                ModuleIdentity::parse("//b").unwrap(),
                ModuleIdentity::parse("//a").unwrap(),
            ]),
        });
        assert_has_help_and_code(&SindriError::NonLibraryDependency {
            dependent: ModuleIdentity::parse("//app").unwrap(),
            dependency: ModuleIdentity::parse("//tools/gen").unwrap(),
            kind: "executable",
        });
        assert_has_help_and_code(&SindriError::NonExecutableModuleTool {
            dependent: ModuleIdentity::parse("//app").unwrap(),
            dependency: ModuleIdentity::parse("//libs/common").unwrap(),
            kind: "library",
        });
        assert_has_help_and_code(&SindriError::ModuleToolBinaryMissing {
            module: ModuleIdentity::parse("//tools/codegen").unwrap(),
            binary: crate::module::ModuleToolReference::parse("//tools/codegen:codegen")
                .unwrap()
                .binary()
                .clone(),
        });
        assert_has_help_and_code(&SindriError::TaskFailed {
            task_name: TaskName::new("go-compile"),
            command: "go build".into(),
            output: "error: undefined".into(),
        });
        assert_has_help_and_code(&SindriError::ScriptEvaluation {
            nickel_message: "missing definition for `program`".into(),
        });
        assert_has_help_and_code(&SindriError::ParameterMissing {
            plugin: PluginName::new("plugin"),
            parameter: ParameterName::new("mode"),
        });
        assert_has_help_and_code(&SindriError::ParameterInvalid {
            plugin: PluginName::new("plugin"),
            parameter: ParameterName::new("mode"),
            nickel_message: "value does not satisfy the contract".into(),
        });
    }
}
