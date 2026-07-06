use crate::module_graph::ModuleGraph;
use crate::module_graph::ModuleNode;
use crate::runtime::Runtime;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::Command;
use crate::types::CommandOutput;
use crate::types::Language;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use smol_str::SmolStr;
use std::borrow::Cow;
use std::io::Result as IoResult;

/// The running Go toolchain's version, as the `go.work` `go` directive wants it: the bare `1.26.4`,
/// not the `go1.26.4` that `go env GOVERSION` reports. It fixes the minimum version the workspace
/// requires; the toolchain is by definition new enough to build every module, so its own version is
/// always a valid directive. The file is regenerated each build and never committed, so tracking the
/// local toolchain has nothing to drift against.
pub struct GoToolchainVersion(SmolStr);

impl GoToolchainVersion {
    /// Ask the toolchain its own version (`go env GOVERSION`). Returns `None` if the command cannot be
    /// run, exits non-zero, or reports a form that does not start with `go` — the caller turns that
    /// into a hard error, since a `go.work` without a valid directive does not build and a build
    /// without a usable `go` is doomed regardless. Sindri does not read `go.mod`, so this is the only
    /// source of the version.
    pub fn query(runtime: &impl Runtime, working_directory: &AbsoluteDirectory) -> Option<GoToolchainVersion> {
        let command: Command = Command::new("go", ["env", "GOVERSION"]);
        let output: CommandOutput = runtime.run_command(&command, working_directory.as_ref()).ok()?;
        if !output.status().is_success() {
            return None;
        }
        GoToolchainVersion::parse(&output.stdout().to_string_lossy())
    }

    fn parse(reported: &str) -> Option<GoToolchainVersion> {
        let version: &str = reported.trim().strip_prefix("go")?;
        if version.is_empty() {
            None
        } else {
            Some(GoToolchainVersion(SmolStr::new(version)))
        }
    }

    fn as_directive(&self) -> &str {
        &self.0
    }
}

/// Go's local-dependency view, projected from the module graph. `go build` resolves an import of a
/// package defined in another workspace module to that module's directory only when a `go.work` lists
/// it under `use`; Sindri generates the file from the `{ module = … }` declarations so those remain the
/// single source of truth and cannot drift from a hand-maintained overlay. It is written into the build
/// directory and reached via the `GOWORK` environment variable, so it is a pure build artifact that
/// never appears in the source tree.
pub struct GoWork {
    toolchain_version: GoToolchainVersion,
    module_directories: Vec<AbsoluteDirectory>,
}

impl GoWork {
    /// Where the generated `go.work` lives: inside the build directory, so it is git-ignored with the
    /// rest of the build output and `GOWORK` can point `go` straight at it.
    pub fn file(build_directory: &AbsoluteDirectory) -> AbsoluteFile {
        build_directory.join_file(&RelativeFile::new("go.work"))
    }

    /// Project the Go modules of `graph` — in its dependency-first node order — into a `go.work`.
    /// Each module's directory is resolved to an absolute path (the file lives in the build directory,
    /// so relative `use` entries would be anchored in the wrong place). Non-Go modules are skipped,
    /// since `go.work` may only `use` directories that contain a `go.mod`.
    pub fn project(
        graph: &ModuleGraph,
        workspace_root: &WorkspaceRoot,
        toolchain_version: GoToolchainVersion,
    ) -> GoWork {
        let module_directories: Vec<AbsoluteDirectory> = graph
            .nodes()
            .iter()
            .filter(|node: &&ModuleNode| -> bool { matches!(node.module().language(), Language::Go) })
            .map(|node: &ModuleNode| -> AbsoluteDirectory {
                workspace_root
                    .to_absolute_directory()
                    .join_directory(node.identity().directory())
            })
            .collect();
        GoWork {
            toolchain_version,
            module_directories,
        }
    }

    /// The `go.work` text: the `go` directive, then a `use ( … )` block naming each module directory by
    /// absolute path, one tab-indented line each.
    pub fn render(&self) -> String {
        let mut rendered: String = String::new();
        rendered.push_str(&format!("go {}\n\n", self.toolchain_version.as_directive()));
        rendered.push_str("use (\n");
        for directory in &self.module_directories {
            rendered.push_str(&format!("\t{}\n", GoWork::use_path(directory)));
        }
        rendered.push_str(")\n");
        rendered
    }

    /// Write the rendered `go.work` into the build directory, creating that directory first so the
    /// write cannot fail merely because no task has created it yet.
    pub fn write(&self, build_directory: &AbsoluteDirectory, runtime: &impl Runtime) -> IoResult<()> {
        runtime.create_directories(build_directory.as_ref())?;
        runtime.write(GoWork::file(build_directory).as_ref(), self.render().as_bytes())
    }

    /// A module directory as a `go.work` `use` path. Any trailing separator is stripped: resolving the
    /// workspace-root module's empty relative directory yields one, and `go.work` does not match a
    /// `use` entry that carries it.
    fn use_path(directory: &AbsoluteDirectory) -> String {
        let path: Cow<'_, str> = directory.as_ref().to_string_lossy();
        path.strip_suffix('/').unwrap_or(&path).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::types::BuildFile;
    use crate::types::CommandOutput;
    use crate::types::Stderr;
    use crate::types::Stdout;
    use crate::types::TaskStatus;
    use crate::types::WorkingDirectory;
    use crate::workspace::Workspace;
    use crate::workspace::WorkspaceConfig;
    use std::path::PathBuf;

    const WORKSPACE: &str = r#"{ name = "test", sindri_version = "0.1.0" }"#;

    fn make_workspace(runtime: &DummyRuntime) -> Workspace {
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let working_directory: WorkingDirectory =
            WorkingDirectory::derive(&workspace_root.to_absolute_directory(), &workspace_root);
        let config: WorkspaceConfig = WorkspaceConfig::load(&workspace_root, runtime).unwrap();
        Workspace::new(workspace_root, working_directory, config)
    }

    /// Load a module graph whose entry is a workspace-root `app` that optionally depends on a local
    /// `//lib/greeting` library, letting the projection tests toggle the declaration on and off.
    fn graph(runtime: &DummyRuntime) -> ModuleGraph {
        let workspace: Workspace = make_workspace(runtime);
        let entry: BuildFile = BuildFile::new(RelativeFile::new("sindri.build"));
        ModuleGraph::load(&entry, &workspace, runtime).unwrap()
    }

    fn runtime_with_dependency() -> DummyRuntime {
        DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//lib/greeting" } ] } }"#,
            )
            .file(
                "/workspace/lib/greeting/sindri.build",
                r#"{ name = "greeting", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .build()
    }

    fn runtime_without_dependency() -> DummyRuntime {
        DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0" }"#,
            )
            .build()
    }

    fn version() -> GoToolchainVersion {
        GoToolchainVersion(SmolStr::new("1.26.4"))
    }

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")))
    }

    fn use_paths(go_work: &GoWork) -> Vec<String> {
        go_work
            .module_directories
            .iter()
            .map(|directory: &AbsoluteDirectory| -> String { directory.as_ref().to_string_lossy().into_owned() })
            .collect()
    }

    #[test]
    fn projects_every_go_module_directory() {
        let runtime: DummyRuntime = runtime_with_dependency();
        let go_work: GoWork = GoWork::project(&graph(&runtime), &workspace_root(), version());
        let directories: Vec<String> = use_paths(&go_work);
        assert_eq!(
            directories.len(),
            2,
            "both modules should be projected, got {directories:?}"
        );
        assert!(
            directories
                .iter()
                .any(|directory: &String| -> bool { directory.ends_with("lib/greeting") }),
            "the library's directory should be projected, got {directories:?}"
        );
    }

    #[test]
    fn removing_the_declaration_removes_the_entry() {
        let with_runtime: DummyRuntime = runtime_with_dependency();
        let with_dependency: GoWork = GoWork::project(&graph(&with_runtime), &workspace_root(), version());
        assert_eq!(with_dependency.module_directories.len(), 2);
        let without_runtime: DummyRuntime = runtime_without_dependency();
        let without_dependency: GoWork = GoWork::project(&graph(&without_runtime), &workspace_root(), version());
        assert_eq!(without_dependency.module_directories.len(), 1);
        assert!(
            !use_paths(&without_dependency)
                .iter()
                .any(|directory: &String| -> bool { directory.ends_with("lib/greeting") }),
            "dropping the dependency should drop its use entry"
        );
    }

    #[test]
    fn render_writes_the_directive_and_absolute_use_paths() {
        let go_work: GoWork = GoWork {
            toolchain_version: version(),
            module_directories: vec![
                AbsoluteDirectory::new(PathBuf::from("/workspace/lib/greeting")),
                AbsoluteDirectory::new(PathBuf::from("/workspace")),
            ],
        };
        let rendered: String = go_work.render();
        assert!(rendered.starts_with("go 1.26.4\n\n"), "got:\n{rendered}");
        assert!(rendered.contains("use (\n"), "got:\n{rendered}");
        assert!(rendered.contains("\t/workspace/lib/greeting\n"), "got:\n{rendered}");
        assert!(rendered.contains("\t/workspace\n"), "got:\n{rendered}");
        assert!(rendered.ends_with(")\n"), "got:\n{rendered}");
    }

    #[test]
    fn render_strips_a_trailing_separator_from_the_root_module() {
        // The workspace-root module's directory resolves with a trailing separator, which go.work does
        // not match; it must be stripped.
        let root_with_separator: PathBuf = PathBuf::from("/workspace").join("");
        let go_work: GoWork = GoWork {
            toolchain_version: version(),
            module_directories: vec![AbsoluteDirectory::new(root_with_separator)],
        };
        let rendered: String = go_work.render();
        assert!(
            rendered.contains("\t/workspace\n"),
            "expected no trailing slash, got:\n{rendered}"
        );
    }

    #[test]
    fn query_parses_the_reported_toolchain_version() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .command(
                "go env GOVERSION",
                CommandOutput::new(
                    Stdout::new(b"go1.26.4\n".to_vec()),
                    Stderr::default(),
                    TaskStatus::Succeeded,
                ),
            )
            .build();
        let version: GoToolchainVersion =
            GoToolchainVersion::query(&runtime, &AbsoluteDirectory::new(PathBuf::from("/workspace"))).unwrap();
        assert_eq!(version.as_directive(), "1.26.4");
    }

    #[test]
    fn query_returns_none_when_the_toolchain_cannot_be_asked() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        assert!(
            GoToolchainVersion::query(&runtime, &AbsoluteDirectory::new(PathBuf::from("/workspace"))).is_none(),
            "an unstubbed go env should leave the version unknown"
        );
    }
}
