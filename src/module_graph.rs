use crate::error::SindriError;
use crate::error::SindriResult;
use crate::module::Module;
use crate::runtime::FileSystem;
use crate::types::BuildFile;
use crate::types::ModuleCycle;
use crate::types::ModuleIdentity;
use crate::workspace::Workspace;
use std::collections::HashSet;

/// The reachable module graph, loaded demand-driven from an entry module. Loading follows
/// each module's `{ module = … }` dependencies transitively — never walking the filesystem — so only
/// modules reachable from the entry point are ever loaded. Modules are recorded in dependency-first
/// order (a module appears after all the modules it depends on), which is the order later phases will
/// build them in.
#[derive(Debug)]
pub struct ModuleGraph {
    modules: Vec<Module>,
}

impl ModuleGraph {
    /// Load the whole graph reachable from `entry`, validating as it goes: a dependency cycle aborts
    /// with the modules in the loop named, and a dependency on a non-`library` module aborts naming both
    /// ends of the offending edge.
    pub fn load(entry: &BuildFile, workspace: &Workspace, file_system: &impl FileSystem) -> SindriResult<ModuleGraph> {
        let mut loader: GraphLoader<'_, _> = GraphLoader {
            workspace,
            file_system,
            loaded: HashSet::new(),
            modules: Vec::new(),
        };
        let mut path: Vec<ModuleIdentity> = Vec::new();
        loader.visit(entry.identity(), None, &mut path)?;
        Ok(ModuleGraph {
            modules: loader.modules,
        })
    }

    /// Every loaded module, in dependency-first order (the entry module is therefore last).
    pub fn modules(&self) -> &[Module] {
        &self.modules
    }
}

/// The mutable state threaded through the depth-first load. `loaded` is the set of modules whose whole
/// subtree is done (so a diamond dependency is loaded once), while the `path` argument of
/// [`visit`](GraphLoader::visit) holds the ancestors currently being loaded — revisiting one of those is
/// a cycle.
struct GraphLoader<'load, FileSystemType: FileSystem> {
    workspace: &'load Workspace,
    file_system: &'load FileSystemType,
    loaded: HashSet<ModuleIdentity>,
    modules: Vec<Module>,
}

impl<FileSystemType: FileSystem> GraphLoader<'_, FileSystemType> {
    fn visit(
        &mut self,
        identity: ModuleIdentity,
        required_by: Option<&ModuleIdentity>,
        path: &mut Vec<ModuleIdentity>,
    ) -> SindriResult<()> {
        if self.loaded.contains(&identity) {
            return Ok(());
        }
        if let Some(position) = path.iter().position(|ancestor: &ModuleIdentity| ancestor == &identity) {
            let mut cycle: Vec<ModuleIdentity> = path[position..].to_vec();
            cycle.push(identity);
            return Err(SindriError::DependencyCycle {
                cycle: ModuleCycle::new(cycle),
            });
        }
        let build_file: BuildFile = identity.to_build_file();
        let module: Module = Module::load(&build_file, self.workspace, self.file_system)?;
        // The entry module (`required_by` is `None`) may be any type; every module reached along a
        // dependency edge must be a library.
        if let Some(dependent) = required_by {
            if !module.artifact_type().is_library() {
                return Err(SindriError::NonLibraryDependency {
                    dependent: dependent.clone(),
                    dependency: identity.clone(),
                    kind: module.artifact_type().name(),
                });
            }
        }
        path.push(identity.clone());
        let dependencies: Vec<ModuleIdentity> = module.dependencies().module_dependencies().cloned().collect();
        for dependency in dependencies {
            self.visit(dependency, Some(&identity), path)?;
        }
        path.pop();
        self.loaded.insert(identity);
        self.modules.push(module);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::types::AbsoluteDirectory;
    use crate::types::RelativeFile;
    use crate::types::WorkingDirectory;
    use crate::types::WorkspaceRoot;
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

    fn entry(relative: &str) -> BuildFile {
        BuildFile::new(RelativeFile::new(relative))
    }

    fn loaded_names(graph: &ModuleGraph) -> Vec<&str> {
        graph
            .modules()
            .iter()
            .map(|loaded: &Module| -> &str { loaded.name().as_ref() })
            .collect()
    }

    #[test]
    fn loads_a_direct_module_dependency() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//libs/common" } ] } }"#,
            )
            .file(
                "/workspace/libs/common/sindri.build",
                r#"{ name = "common", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime);
        let graph: ModuleGraph = ModuleGraph::load(&entry("sindri.build"), &workspace, &runtime).unwrap();
        let names: Vec<&str> = loaded_names(&graph);
        assert!(names.contains(&"app"), "expected the entry module, got {names:?}");
        assert!(names.contains(&"common"), "expected the dependency, got {names:?}");
    }

    #[test]
    fn loads_a_transitive_chain() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/a/sindri.build",
                r#"{ name = "a", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//b" } ] } }"#,
            )
            .file(
                "/workspace/b/sindri.build",
                r#"{ name = "b", language = "go", type = "library", version = "0.1.0",
                     dependencies = { compile = [ { module = "//c" } ] } }"#,
            )
            .file(
                "/workspace/c/sindri.build",
                r#"{ name = "c", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime);
        let graph: ModuleGraph = ModuleGraph::load(&entry("a/sindri.build"), &workspace, &runtime).unwrap();
        let names: Vec<&str> = loaded_names(&graph);
        assert!(
            names.contains(&"a") && names.contains(&"b") && names.contains(&"c"),
            "expected all three modules in the chain, got {names:?}"
        );
        // Dependency-first order: the deepest module is loaded before the ones that depend on it.
        assert_eq!(names, vec!["c", "b", "a"]);
    }

    #[test]
    fn loads_a_shared_dependency_once() {
        // A diamond: the entry depends on both `a` and `b`, and each depends on `shared`. `shared`
        // must be loaded exactly once (the second path hits the already-loaded short-circuit) and the
        // convergence must not be mistaken for a cycle.
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//a" }, { module = "//b" } ] } }"#,
            )
            .file(
                "/workspace/a/sindri.build",
                r#"{ name = "a", language = "go", type = "library", version = "0.1.0",
                     dependencies = { compile = [ { module = "//shared" } ] } }"#,
            )
            .file(
                "/workspace/b/sindri.build",
                r#"{ name = "b", language = "go", type = "library", version = "0.1.0",
                     dependencies = { compile = [ { module = "//shared" } ] } }"#,
            )
            .file(
                "/workspace/shared/sindri.build",
                r#"{ name = "shared", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime);
        let graph: ModuleGraph = ModuleGraph::load(&entry("sindri.build"), &workspace, &runtime).unwrap();
        let names: Vec<&str> = loaded_names(&graph);
        assert_eq!(
            names.iter().filter(|name: &&&str| **name == "shared").count(),
            1,
            "the shared dependency must be loaded exactly once, got {names:?}"
        );
    }

    #[test]
    fn does_not_load_unreachable_modules() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//libs/common" } ] } }"#,
            )
            .file(
                "/workspace/libs/common/sindri.build",
                r#"{ name = "common", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .file(
                "/workspace/libs/unused/sindri.build",
                r#"{ name = "unused", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime);
        let graph: ModuleGraph = ModuleGraph::load(&entry("sindri.build"), &workspace, &runtime).unwrap();
        let names: Vec<&str> = loaded_names(&graph);
        assert!(
            names.contains(&"common"),
            "expected the reachable module, got {names:?}"
        );
        assert!(
            !names.contains(&"unused"),
            "the unreachable module must never be loaded, got {names:?}"
        );
    }

    #[test]
    fn rejects_a_dependency_cycle() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/a/sindri.build",
                r#"{ name = "a", language = "go", type = "library", version = "0.1.0",
                     dependencies = { compile = [ { module = "//b" } ] } }"#,
            )
            .file(
                "/workspace/b/sindri.build",
                r#"{ name = "b", language = "go", type = "library", version = "0.1.0",
                     dependencies = { compile = [ { module = "//a" } ] } }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime);
        let error: SindriError = ModuleGraph::load(&entry("a/sindri.build"), &workspace, &runtime).unwrap_err();
        match error {
            SindriError::DependencyCycle { ref cycle } => {
                // The path closes on itself: the entry `//a`, then `//b`, then back to `//a`.
                assert_eq!(cycle.to_string(), "//a → //b → //a");
            }
            other => panic!("expected a dependency-cycle error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_a_non_library_dependency() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", WORKSPACE)
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//tools/gen" } ] } }"#,
            )
            .file(
                "/workspace/tools/gen/sindri.build",
                r#"{ name = "gen", language = "go", type = "executable", version = "0.1.0" }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime);
        let error: SindriError = ModuleGraph::load(&entry("sindri.build"), &workspace, &runtime).unwrap_err();
        assert!(
            matches!(error, SindriError::NonLibraryDependency { .. }),
            "expected a non-library-dependency error, got {error:?}"
        );
    }
}
