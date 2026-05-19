use crate::error::SindriError;
use crate::error::SindriResult;
use crate::nickel_eval::Contract;
use crate::nickel_eval::Nickel;
use crate::runtime::FileSystem;
use crate::types::{
    AbsoluteDirectory, AbsoluteFile, BuildFile, ConfigFile, DirEntry, FileKind, Language, ModuleName, RelativeFile,
    Version, WorkspaceRoot,
};
use crate::workspace::Workspace;
use nickel_lang::Expr;
use serde::Deserialize;
use std::borrow::Cow;

const MODULE_CONTRACT: Contract = Contract::new(
    include_str!("contracts/module.ncl"),
    r#"{
  name = "my-module",
  language = "go",
  type = "executable",
  version = "0.1.0",
}"#,
);

#[derive(Debug)]
pub enum ArtifactType {
    Library,
    Executable,
    WebArchive,
    ContainerImage,
}

impl<'de> Deserialize<'de> for ArtifactType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value: String = String::deserialize(deserializer)?;
        match value.as_str() {
            "library" => Ok(ArtifactType::Library),
            "executable" => Ok(ArtifactType::Executable),
            "web-archive" => Ok(ArtifactType::WebArchive),
            "container-image" => Ok(ArtifactType::ContainerImage),
            other => Err(serde::de::Error::custom(format!(
                "unknown artifact type `{other}`; expected one of: library, executable, web-archive, container-image"
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Module {
    name: ModuleName,
    language: Language,
    #[serde(rename = "type")]
    artifact_type: ArtifactType,
    version: Version,
}

impl Module {
    pub fn name(&self) -> &ModuleName {
        &self.name
    }

    pub fn language(&self) -> &Language {
        &self.language
    }

    pub fn artifact_type(&self) -> &ArtifactType {
        &self.artifact_type
    }

    pub fn version(&self) -> &Version {
        &self.version
    }
}

impl BuildFile {
    pub fn find(workspace: &Workspace, file_system: &impl FileSystem) -> SindriResult<BuildFile> {
        let workspace_root: &WorkspaceRoot = workspace.workspace_root();
        let mut current: AbsoluteDirectory = workspace.absolute_working_directory();
        let root_directory: AbsoluteDirectory = workspace_root.to_absolute_directory();
        let default_build_file_name: RelativeFile = RelativeFile::new("sindri.build");
        loop {
            let default_build_file: AbsoluteFile = current.join_file(&default_build_file_name);
            let relative_build_file: RelativeFile = workspace_root.relativize_file(&default_build_file);
            let default_kind: Option<FileKind> =
                file_system
                    .file_kind(default_build_file.as_ref())
                    .map_err(|source| SindriError::Io {
                        path: relative_build_file.as_ref().to_path_buf(),
                        source,
                    })?;
            match default_kind {
                Some(FileKind::Symlink) => {
                    return Err(SindriError::SymlinkNotSupported {
                        path: relative_build_file.as_ref().to_path_buf(),
                    });
                }
                Some(FileKind::File) => {
                    return Ok(BuildFile::new(relative_build_file));
                }
                Some(FileKind::Directory) | None => {}
            }
            let entries: Vec<DirEntry> =
                file_system
                    .read_directory(current.as_ref())
                    .map_err(|source| SindriError::Io {
                        path: workspace_root.relativize_directory(&current).as_ref().to_path_buf(),
                        source,
                    })?;
            for entry in &entries {
                let name: Cow<'_, str> = entry.file_name();
                if name.starts_with("sindri-") && name.ends_with(".build") {
                    let entry_file: AbsoluteFile = AbsoluteFile::new(entry.path().to_path_buf());
                    let relative_entry: RelativeFile = workspace_root.relativize_file(&entry_file);
                    if entry.kind() == FileKind::Symlink {
                        return Err(SindriError::SymlinkNotSupported {
                            path: relative_entry.as_ref().to_path_buf(),
                        });
                    }
                    return Ok(BuildFile::new(relative_entry));
                }
            }
            if current == root_directory {
                break;
            }
            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
        Err(SindriError::ModuleNotFound {
            start: workspace.working_directory().as_ref().to_path_buf(),
        })
    }
}

impl Module {
    pub fn load(build_file: &BuildFile, workspace: &Workspace, file_system: &impl FileSystem) -> SindriResult<Module> {
        let config_file: ConfigFile = build_file.config_file(workspace.workspace_root());
        let expression: Expr = Nickel::evaluate_with_contract(&config_file, &MODULE_CONTRACT, file_system)?;
        expression.to_serde::<Module>().map_err(|source| SindriError::Schema {
            path: config_file.workspace_path().as_ref().to_path_buf(),
            message: source.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SindriError;
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::types::WorkingDirectory;
    use crate::workspace::WorkspaceConfig;
    use std::path::Path;
    use std::path::PathBuf;

    const WORKSPACE: &str = r#"{ name = "test", sindri_version = "0.1.0" }"#;
    const MINIMAL: &str = r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0" }"#;

    fn workspace_runtime() -> DummyRuntimeBuilder {
        DummyRuntime::builder().file("/workspace/sindri.workspace", WORKSPACE)
    }

    fn make_workspace(runtime: &DummyRuntime, working_subdirectory: &str) -> Workspace {
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let absolute_subdirectory: AbsoluteDirectory =
            AbsoluteDirectory::new(workspace_root.as_ref().join(working_subdirectory));
        let working_directory: WorkingDirectory = WorkingDirectory::derive(&absolute_subdirectory, &workspace_root);
        let config: WorkspaceConfig = WorkspaceConfig::load(&workspace_root, runtime).unwrap();
        Workspace::new(workspace_root, working_directory, config)
    }

    #[test]
    fn find_build_file_in_current_directory() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::find(&workspace, &runtime).unwrap();
        assert_eq!(build_file.as_ref(), Path::new("sindri.build"));
    }

    #[test]
    fn find_build_file_in_parent_directory() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri.build", MINIMAL)
            .directory("/workspace/src")
            .build();
        let workspace: Workspace = make_workspace(&runtime, "src");
        let build_file: BuildFile = BuildFile::find(&workspace, &runtime).unwrap();
        assert_eq!(build_file.as_ref(), Path::new("sindri.build"));
    }

    #[test]
    fn find_build_file_qualified_build_file() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri-kotlin.build", MINIMAL)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::find(&workspace, &runtime).unwrap();
        assert_eq!(build_file.as_ref(), Path::new("sindri-kotlin.build"));
    }

    #[test]
    fn find_build_file_stops_at_workspace_root() {
        let runtime: DummyRuntime = workspace_runtime().directory("/workspace/src").build();
        let workspace: Workspace = make_workspace(&runtime, "src");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::ModuleNotFound { .. }));
    }

    #[test]
    fn find_build_file_not_found() {
        let runtime: DummyRuntime = workspace_runtime().build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::ModuleNotFound { .. }));
    }

    #[test]
    fn find_build_file_rejects_symlink() {
        let runtime: DummyRuntime = workspace_runtime().symlink("/workspace/sindri.build").build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::SymlinkNotSupported { .. }));
    }

    #[test]
    fn load_valid_module() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        assert_eq!(module.name().as_ref(), "my-app");
        assert!(matches!(module.language(), Language::Go));
        assert!(matches!(module.artifact_type(), ArtifactType::Executable));
        assert_eq!(module.version().as_ref(), "0.1.0");
    }

    #[test]
    fn load_module_syntax_error() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri.build", r#"{ name = "test", language = }"#)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_missing_required_field() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri.build", r#"{ name = "test", language = "go" }"#)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_contract_type_violation() {
        let runtime: DummyRuntime = workspace_runtime()
            .file(
                "/workspace/sindri.build",
                r#"{ name = 42, language = "go", type = "executable", version = "0.1.0" }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_passes_contract() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new("sindri.build"));
        Module::load(&build_file, &workspace, &runtime).unwrap();
    }
}
