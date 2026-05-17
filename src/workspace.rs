use crate::error::SindriError;
use crate::nickel_eval;
use crate::types::{BuildDirectory, Repository, Version, WorkspaceFile, WorkspaceName, WorkspaceRoot};
use nickel_lang::Expr;
use serde::Deserialize;
use std::path::{Path, PathBuf};

const WORKSPACE_CONTRACT: &str = include_str!("contracts/workspace.ncl");
const WORKSPACE_MINIMAL_EXAMPLE: &str = r#"{
  name = "my-project",
  sindri_version = "0.1.0",
}"#;

#[derive(Debug, Deserialize)]
pub struct PluginRef {
    pub name: String,
    pub version: Version,
}

#[derive(Debug, Deserialize)]
pub struct Workspace {
    pub name: WorkspaceName,
    pub sindri_version: Version,
    #[serde(default = "default_build_dir")]
    pub build_dir: BuildDirectory,
    #[serde(default)]
    pub plugins: Vec<PluginRef>,
    #[serde(default)]
    pub repositories: Vec<Repository>,
}

fn default_build_dir() -> BuildDirectory {
    BuildDirectory::new(PathBuf::from(".target"))
}

pub fn find_root(start: &Path) -> Result<WorkspaceRoot, SindriError> {
    let mut current: PathBuf = start.to_path_buf();
    loop {
        let workspace_file: PathBuf = current.join("sindri.workspace");
        if workspace_file.is_symlink() {
            return Err(SindriError::SymlinkNotSupported {
                path: workspace_file.display().to_string(),
            });
        }
        if workspace_file.is_file() {
            return Ok(WorkspaceRoot::new(current));
        }
        if !current.pop() {
            return Err(SindriError::WorkspaceNotFound {
                start: start.display().to_string(),
            });
        }
    }
}

pub fn load(workspace_root: &WorkspaceRoot) -> Result<Workspace, SindriError> {
    let workspace_file: WorkspaceFile = workspace_root.workspace_file();
    let expression: Expr = nickel_eval::evaluate_file_with_contract(
        workspace_file.as_ref(),
        WORKSPACE_CONTRACT,
        WORKSPACE_MINIMAL_EXAMPLE,
    )?;
    expression
        .to_serde::<Workspace>()
        .map_err(|source| SindriError::Schema {
            path: workspace_file.to_string(),
            message: source.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SindriError;
    use std::fs;
    use std::path::Path;
    use tempfile::TempDir;

    const MINIMAL: &str = r#"{ name = "test-project", sindri_version = "0.1.0" }"#;

    fn write_workspace(directory: &TempDir, content: &str) {
        fs::write(directory.path().join("sindri.workspace"), content).unwrap();
    }

    #[test]
    fn find_root_from_workspace_directory() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(&directory, MINIMAL);
        let root: WorkspaceRoot = find_root(directory.path()).unwrap();
        assert_eq!(root.as_ref(), directory.path());
    }

    #[test]
    fn find_root_from_subdirectory() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(&directory, MINIMAL);
        let subdirectory: PathBuf = directory.path().join("src").join("main");
        fs::create_dir_all(&subdirectory).unwrap();
        let root: WorkspaceRoot = find_root(&subdirectory).unwrap();
        assert_eq!(root.as_ref(), directory.path());
    }

    #[test]
    fn find_root_not_found() {
        let directory: TempDir = TempDir::new().unwrap();
        let error: SindriError = find_root(directory.path()).unwrap_err();
        assert!(matches!(error, SindriError::WorkspaceNotFound { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn find_root_rejects_symlink() {
        use std::os::unix::fs::symlink;
        let directory: TempDir = TempDir::new().unwrap();
        let target: TempDir = TempDir::new().unwrap();
        fs::write(target.path().join("real"), MINIMAL).unwrap();
        symlink(target.path().join("real"), directory.path().join("sindri.workspace")).unwrap();
        let error: SindriError = find_root(directory.path()).unwrap_err();
        assert!(matches!(error, SindriError::SymlinkNotSupported { .. }));
    }

    #[test]
    fn load_valid_workspace() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(&directory, MINIMAL);
        let root: WorkspaceRoot = find_root(directory.path()).unwrap();
        let workspace: Workspace = load(&root).unwrap();
        assert_eq!(workspace.name.as_ref(), "test-project");
        assert_eq!(workspace.sindri_version.as_ref(), "0.1.0");
        assert_eq!(workspace.build_dir.as_ref(), Path::new(".target"));
        assert!(workspace.plugins.is_empty());
        assert!(workspace.repositories.is_empty());
    }

    #[test]
    fn load_workspace_with_custom_build_dir() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(
            &directory,
            r#"{ name = "test", sindri_version = "0.1.0", build_dir = "build" }"#,
        );
        let root: WorkspaceRoot = find_root(directory.path()).unwrap();
        let workspace: Workspace = load(&root).unwrap();
        assert_eq!(workspace.build_dir.as_ref(), Path::new("build"));
    }

    #[test]
    fn load_workspace_syntax_error() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(&directory, r#"{ name = "test", sindri_version = }"#);
        let root: WorkspaceRoot = WorkspaceRoot::new(directory.path().to_path_buf());
        let error: SindriError = load(&root).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_workspace_missing_required_field() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(&directory, r#"{ name = "test" }"#);
        let root: WorkspaceRoot = WorkspaceRoot::new(directory.path().to_path_buf());
        let error: SindriError = load(&root).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_workspace_contract_type_violation() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(&directory, r#"{ name = 42, sindri_version = "0.1.0" }"#);
        let root: WorkspaceRoot = WorkspaceRoot::new(directory.path().to_path_buf());
        let error: SindriError = load(&root).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_workspace_passes_contract() {
        let directory: TempDir = TempDir::new().unwrap();
        write_workspace(&directory, MINIMAL);
        let root: WorkspaceRoot = find_root(directory.path()).unwrap();
        load(&root).unwrap();
    }
}
