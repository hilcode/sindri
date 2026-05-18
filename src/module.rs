use crate::error::SindriError;
use crate::nickel_eval;
use crate::types::{BuildFile, Language, ModuleName, Version, WorkspaceRoot};
use nickel_lang::Expr;
use serde::Deserialize;
use std::borrow::Cow;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const MODULE_CONTRACT: &str = include_str!("contracts/module.ncl");
const MODULE_MINIMAL_EXAMPLE: &str = r#"{
  name = "my-module",
  language = "go",
  type = "executable",
  version = "0.1.0",
}"#;

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
    pub name: ModuleName,
    pub language: Language,
    #[serde(rename = "type")]
    pub artifact_type: ArtifactType,
    pub version: Version,
}

pub fn find_entry_point(start: &Path, workspace_root: &WorkspaceRoot) -> Result<BuildFile, SindriError> {
    let mut current: PathBuf = start.to_path_buf();
    loop {
        let default_build_file: PathBuf = current.join("sindri.build");
        if default_build_file.is_symlink() {
            return Err(SindriError::SymlinkNotSupported {
                path: default_build_file.display().to_string(),
            });
        }
        if default_build_file.is_file() {
            return Ok(BuildFile::new(default_build_file));
        }
        if let Ok(entries) = std::fs::read_dir(&current) {
            for entry in entries.flatten() {
                let file_name: OsString = entry.file_name();
                let name: Cow<'_, str> = file_name.to_string_lossy();
                if name.starts_with("sindri-") && name.ends_with(".build") {
                    let path: PathBuf = entry.path();
                    if path.is_symlink() {
                        return Err(SindriError::SymlinkNotSupported {
                            path: path.display().to_string(),
                        });
                    }
                    return Ok(BuildFile::new(path));
                }
            }
        }
        if current == workspace_root.as_ref() {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    Err(SindriError::ModuleNotFound {
        start: start.display().to_string(),
    })
}

pub fn load(build_file: &BuildFile) -> Result<Module, SindriError> {
    let expression: Expr =
        nickel_eval::evaluate_file_with_contract(build_file.as_ref(), MODULE_CONTRACT, MODULE_MINIMAL_EXAMPLE)?;
    expression.to_serde::<Module>().map_err(|source| SindriError::Schema {
        path: build_file.to_string(),
        message: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SindriError;
    use std::fs;
    use tempfile::TempDir;

    const MINIMAL: &str = r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0" }"#;

    fn make_workspace_root(directory: &TempDir) -> WorkspaceRoot {
        fs::write(
            directory.path().join("sindri.workspace"),
            r#"{ name = "test", sindri_version = "0.1.0" }"#,
        )
        .unwrap();
        WorkspaceRoot::new(directory.path().to_path_buf())
    }

    fn write_build_file(directory: &TempDir, content: &str) {
        fs::write(directory.path().join("sindri.build"), content).unwrap();
    }

    #[test]
    fn find_entry_point_in_current_directory() {
        let directory: TempDir = TempDir::new().unwrap();
        let workspace_root: WorkspaceRoot = make_workspace_root(&directory);
        write_build_file(&directory, MINIMAL);
        let build_file: BuildFile = find_entry_point(directory.path(), &workspace_root).unwrap();
        assert_eq!(build_file.as_ref(), directory.path().join("sindri.build"));
    }

    #[test]
    fn find_entry_point_in_parent_directory() {
        let directory: TempDir = TempDir::new().unwrap();
        let workspace_root: WorkspaceRoot = make_workspace_root(&directory);
        write_build_file(&directory, MINIMAL);
        let subdirectory: PathBuf = directory.path().join("src");
        fs::create_dir_all(&subdirectory).unwrap();
        let build_file: BuildFile = find_entry_point(&subdirectory, &workspace_root).unwrap();
        assert_eq!(build_file.as_ref(), directory.path().join("sindri.build"));
    }

    #[test]
    fn find_entry_point_qualified_build_file() {
        let directory: TempDir = TempDir::new().unwrap();
        let workspace_root: WorkspaceRoot = make_workspace_root(&directory);
        fs::write(directory.path().join("sindri-kotlin.build"), MINIMAL).unwrap();
        let build_file: BuildFile = find_entry_point(directory.path(), &workspace_root).unwrap();
        assert_eq!(build_file.as_ref(), directory.path().join("sindri-kotlin.build"));
    }

    #[test]
    fn find_entry_point_stops_at_workspace_root() {
        let directory: TempDir = TempDir::new().unwrap();
        let workspace_root: WorkspaceRoot = make_workspace_root(&directory);
        let subdirectory: PathBuf = directory.path().join("src");
        fs::create_dir_all(&subdirectory).unwrap();
        let error: SindriError = find_entry_point(&subdirectory, &workspace_root).unwrap_err();
        assert!(matches!(error, SindriError::ModuleNotFound { .. }));
    }

    #[test]
    fn find_entry_point_not_found() {
        let directory: TempDir = TempDir::new().unwrap();
        let workspace_root: WorkspaceRoot = make_workspace_root(&directory);
        let error: SindriError = find_entry_point(directory.path(), &workspace_root).unwrap_err();
        assert!(matches!(error, SindriError::ModuleNotFound { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn find_entry_point_rejects_symlink() {
        use std::os::unix::fs::symlink;
        let directory: TempDir = TempDir::new().unwrap();
        let workspace_root: WorkspaceRoot = make_workspace_root(&directory);
        let target: TempDir = TempDir::new().unwrap();
        fs::write(target.path().join("real"), MINIMAL).unwrap();
        symlink(target.path().join("real"), directory.path().join("sindri.build")).unwrap();
        let error: SindriError = find_entry_point(directory.path(), &workspace_root).unwrap_err();
        assert!(matches!(error, SindriError::SymlinkNotSupported { .. }));
    }

    #[test]
    fn load_valid_module() {
        let directory: TempDir = TempDir::new().unwrap();
        write_build_file(&directory, MINIMAL);
        let build_file: BuildFile = BuildFile::new(directory.path().join("sindri.build"));
        let module: Module = load(&build_file).unwrap();
        assert_eq!(module.name.as_ref(), "my-app");
        assert!(matches!(module.language, Language::Go));
        assert!(matches!(module.artifact_type, ArtifactType::Executable));
        assert_eq!(module.version.as_ref(), "0.1.0");
    }

    #[test]
    fn load_module_syntax_error() {
        let directory: TempDir = TempDir::new().unwrap();
        write_build_file(&directory, r#"{ name = "test", language = }"#);
        let build_file: BuildFile = BuildFile::new(directory.path().join("sindri.build"));
        let error: SindriError = load(&build_file).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_missing_required_field() {
        let directory: TempDir = TempDir::new().unwrap();
        write_build_file(&directory, r#"{ name = "test", language = "go" }"#);
        let build_file: BuildFile = BuildFile::new(directory.path().join("sindri.build"));
        let error: SindriError = load(&build_file).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_contract_type_violation() {
        let directory: TempDir = TempDir::new().unwrap();
        write_build_file(
            &directory,
            r#"{ name = 42, language = "go", type = "executable", version = "0.1.0" }"#,
        );
        let build_file: BuildFile = BuildFile::new(directory.path().join("sindri.build"));
        let error: SindriError = load(&build_file).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_passes_contract() {
        let directory: TempDir = TempDir::new().unwrap();
        write_build_file(&directory, MINIMAL);
        let build_file: BuildFile = BuildFile::new(directory.path().join("sindri.build"));
        load(&build_file).unwrap();
    }
}
