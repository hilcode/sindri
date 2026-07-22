use crate::error::SindriError;
use crate::error::SindriResult;
use crate::nickel_eval::Contract;
use crate::nickel_eval::Nickel;
use crate::parameter::PluginName;
use crate::runtime::FileSystem;
use crate::runtime::Runtime;
use crate::types::{
    AbsoluteDirectory, AbsoluteFile, BuildDirectory, ConfigFile, FileKind, RelativeDirectory, RelativeFile, Version,
    WorkingDirectory, WorkspaceName, WorkspaceRoot,
};
use nickel_lang::Expr;
use serde::Deserialize;
use std::path::PathBuf;

/// `stdlib.ncl` is a `let ... in` fragment, so it must precede the record it scopes over rather than
/// be concatenated after it; `concat!` splices the two files at compile time into one contract source,
/// the same way [`crate::script::Script::evaluate`] splices it in front of the command contract.
const WORKSPACE_CONTRACT: Contract = Contract::new(
    concat!(
        include_str!("contracts/stdlib.ncl"),
        include_str!("contracts/workspace.ncl")
    ),
    r#"{
  name = "my-project",
  sindri_version = "0.1.0",
}"#,
);

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct PluginRef {
    name: PluginName,
    version: Version,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceConfig {
    name: WorkspaceName,
    sindri_version: Version,
    #[serde(default = "default_build_directory")]
    build_directory: BuildDirectory,
    #[serde(default)]
    plugins: Vec<PluginRef>,
}

impl WorkspaceConfig {
    pub fn name(&self) -> &WorkspaceName {
        &self.name
    }

    pub fn sindri_version(&self) -> &Version {
        &self.sindri_version
    }

    pub fn build_directory(&self) -> &BuildDirectory {
        &self.build_directory
    }

    pub fn plugins(&self) -> &[PluginRef] {
        &self.plugins
    }

    pub fn load(workspace_root: &WorkspaceRoot, file_system: &impl FileSystem) -> SindriResult<WorkspaceConfig> {
        let config_file: ConfigFile = ConfigFile::resolve(
            RelativeFile::new("sindri.workspace").expect("a literal file name is always well-formed"),
            workspace_root,
        );
        let expression: Expr = Nickel::evaluate_with_contract(&config_file, &WORKSPACE_CONTRACT, file_system)?;
        expression
            .to_serde::<WorkspaceConfig>()
            .map_err(|source| SindriError::Schema {
                path: config_file.workspace_path().as_ref().to_path_buf(),
                message: source.to_string(),
            })
    }
}

fn default_build_directory() -> BuildDirectory {
    BuildDirectory::new(RelativeDirectory::new(".target/").expect("a literal directory name is always well-formed"))
}

pub struct Workspace {
    workspace_root: WorkspaceRoot,
    working_directory: WorkingDirectory,
    config: WorkspaceConfig,
}

impl Workspace {
    pub fn new(
        workspace_root: WorkspaceRoot,
        working_directory: WorkingDirectory,
        config: WorkspaceConfig,
    ) -> Workspace {
        Workspace {
            workspace_root,
            working_directory,
            config,
        }
    }

    pub fn locate(file_system: &impl FileSystem) -> SindriResult<Workspace> {
        let absolute_cwd: AbsoluteDirectory =
            AbsoluteDirectory::new(file_system.current_directory().map_err(|source| SindriError::Io {
                path: PathBuf::new(),
                source,
            })?);
        let workspace_root: WorkspaceRoot = WorkspaceRoot::find(&absolute_cwd, file_system)?;
        let working_directory: WorkingDirectory = WorkingDirectory::derive(&absolute_cwd, &workspace_root);
        let config: WorkspaceConfig = WorkspaceConfig::load(&workspace_root, file_system)?;
        Ok(Workspace {
            workspace_root,
            working_directory,
            config,
        })
    }

    /// Record that this workspace was loaded. Kept separate from [`Workspace::locate`] because the
    /// log sink lives inside this workspace's build directory and is only opened once the workspace
    /// is known — so the message has to be emitted after logging is enabled, not during discovery.
    pub fn log_loaded(&self, runtime: &impl Runtime) -> SindriResult<()> {
        runtime
            .log(&format!("Workspace loaded: {}", self.workspace_root))
            .map_err(|source| SindriError::Log { source })
    }

    pub fn workspace_root(&self) -> &WorkspaceRoot {
        &self.workspace_root
    }

    pub fn working_directory(&self) -> &WorkingDirectory {
        &self.working_directory
    }

    pub fn config(&self) -> &WorkspaceConfig {
        &self.config
    }

    /// The working directory resolved against this workspace's root.
    pub fn absolute_working_directory(&self) -> AbsoluteDirectory {
        self.working_directory.absolute(&self.workspace_root)
    }

    /// The configured build directory resolved against this workspace's root.
    pub fn absolute_build_directory(&self) -> AbsoluteDirectory {
        self.config.build_directory().absolute(&self.workspace_root)
    }
}

impl WorkspaceRoot {
    pub fn find(start: &AbsoluteDirectory, file_system: &impl FileSystem) -> SindriResult<WorkspaceRoot> {
        let workspace_file_name: RelativeFile =
            RelativeFile::new("sindri.workspace").expect("a literal file name is always well-formed");
        let mut current: AbsoluteDirectory = start.clone();
        loop {
            let workspace_file: AbsoluteFile = current.join_file(&workspace_file_name);
            let kind: Option<FileKind> =
                file_system
                    .file_kind(workspace_file.as_ref())
                    .map_err(|source| SindriError::Io {
                        path: workspace_file.as_ref().to_path_buf(),
                        source,
                    })?;
            match kind {
                Some(FileKind::Symlink) => {
                    return Err(SindriError::SymlinkNotSupported {
                        path: PathBuf::from("sindri.workspace"),
                    });
                }
                Some(FileKind::File) => return Ok(WorkspaceRoot::new(current)),
                Some(FileKind::Directory) | None => {}
            }
            match current.parent() {
                Some(parent) => current = parent,
                None => {
                    return Err(SindriError::WorkspaceNotFound {
                        start: start.as_ref().to_path_buf(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SindriError;
    use crate::runtime::DummyRuntime;
    use std::io::ErrorKind;
    use std::path::Path;

    const MINIMAL: &str = r#"{ name = "test-project", sindri_version = "0.1.0" }"#;

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")))
    }

    fn load(content: &str) -> SindriResult<WorkspaceConfig> {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", content)
            .build();
        WorkspaceConfig::load(&workspace_root(), &runtime)
    }

    #[test]
    fn find_workspace_root_from_workspace_directory() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", MINIMAL)
            .build();
        let root: WorkspaceRoot =
            WorkspaceRoot::find(&AbsoluteDirectory::new(PathBuf::from("/workspace")), &runtime).unwrap();
        assert_eq!(root.as_ref(), Path::new("/workspace"));
    }

    #[test]
    fn find_workspace_root_from_subdirectory() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", MINIMAL)
            .build();
        let root: WorkspaceRoot =
            WorkspaceRoot::find(&AbsoluteDirectory::new(PathBuf::from("/workspace/src/main")), &runtime).unwrap();
        assert_eq!(root.as_ref(), Path::new("/workspace"));
    }

    #[test]
    fn find_workspace_root_not_found() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let error: SindriError =
            WorkspaceRoot::find(&AbsoluteDirectory::new(PathBuf::from("/workspace")), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::WorkspaceNotFound { .. }));
    }

    #[test]
    fn find_workspace_root_rejects_symlink() {
        let runtime: DummyRuntime = DummyRuntime::builder().symlink("/workspace/sindri.workspace").build();
        let error: SindriError =
            WorkspaceRoot::find(&AbsoluteDirectory::new(PathBuf::from("/workspace")), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::SymlinkNotSupported { .. }));
    }

    #[test]
    fn find_workspace_root_reports_io_errors_from_file_kind() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .directory("/workspace")
            .error("/workspace/sindri.workspace", ErrorKind::PermissionDenied)
            .build();
        let error: SindriError =
            WorkspaceRoot::find(&AbsoluteDirectory::new(PathBuf::from("/workspace")), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }));
    }

    #[test]
    fn load_valid_workspace_config() {
        let config: WorkspaceConfig = load(MINIMAL).unwrap();
        assert_eq!(config.name().as_ref(), "test-project");
        assert_eq!(config.sindri_version().as_ref(), "0.1.0");
        assert_eq!(config.build_directory().as_ref(), Path::new(".target/"));
        assert!(config.plugins().is_empty());
    }

    #[test]
    fn load_workspace_config_with_custom_build_directory() {
        let config: WorkspaceConfig =
            load(r#"{ name = "test", sindri_version = "0.1.0", build_directory = "build/" }"#).unwrap();
        assert_eq!(config.build_directory().as_ref(), Path::new("build/"));
    }

    #[test]
    fn load_workspace_config_rejects_an_absolute_build_directory() {
        let error: SindriError =
            load(r#"{ name = "test", sindri_version = "0.1.0", build_directory = "/etc" }"#).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
        assert!(
            error.to_string().contains("must not start with"),
            "message was: {error}"
        );
    }

    #[test]
    fn load_workspace_config_syntax_error() {
        let error: SindriError = load(r#"{ name = "test", sindri_version = }"#).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_workspace_config_missing_required_field() {
        let error: SindriError = load(r#"{ name = "test" }"#).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_workspace_config_contract_type_violation() {
        let error: SindriError = load(r#"{ name = 42, sindri_version = "0.1.0" }"#).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_workspace_config_passes_contract() {
        load(MINIMAL).unwrap();
    }

    #[test]
    fn locate_finds_the_workspace_from_the_current_directory() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", MINIMAL)
            .current_directory("/workspace")
            .build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        assert_eq!(workspace.workspace_root().as_ref(), Path::new("/workspace"));
        assert_eq!(workspace.config().name().as_ref(), "test-project");
    }

    #[test]
    fn locate_reports_io_errors_from_current_directory() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .current_directory_error(ErrorKind::PermissionDenied)
            .build();
        let result: SindriResult<Workspace> = Workspace::locate(&runtime);
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn log_loaded_records_the_workspace_root() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", MINIMAL)
            .build();
        let config: WorkspaceConfig = WorkspaceConfig::load(&workspace_root(), &runtime).unwrap();
        let workspace: Workspace = Workspace::new(
            workspace_root(),
            WorkingDirectory::new(RelativeDirectory::new_unchecked(PathBuf::new())),
            config,
        );
        workspace.log_loaded(&runtime).unwrap();
        assert_eq!(runtime.logged(), vec!["Workspace loaded: /workspace/".to_string()]);
    }
}
