use crate::file_set::FileSetPattern;
use crate::module::ArtifactType;
use crate::module_graph::ModuleGraph;
use crate::module_graph::ModuleNode;
use crate::parameter::ParameterDeclarations;
use crate::plugin::Plugin;
use crate::plugins::PluginRegistry;
use crate::script::Script;
use crate::task::DeclaredTaskInput;
use crate::task::ManagedTaskInput;
use crate::task::Task;
use crate::task::TaskName;
use crate::task::TaskOutput;
use crate::types::AbsoluteDirectory;
use crate::types::Language;
use crate::types::Step;
use crate::types::WorkspaceRoot;

/// The built-in Go plugin: `generate-go-work` (Rust-side glue a manifest-authored task can't yet
/// express — see [`GoPlugin::generate_go_work_task`]) plus the tasks loaded from
/// `.sindri/plugins/go/` that [`GoPlugin::tasks_for`] projects onto a given module.
pub struct GoPlugin;

impl GoPlugin {
    /// This build's tasks for a module of `artifact_type`: every task every loaded plugin
    /// contributes, applied unconditionally regardless of the module's own [`Language`] — there is
    /// only one known plugin today, so this is not yet a per-language dispatch. `go-package` links a
    /// runnable binary into the tracked output directory, so it's filtered out for anything but an
    /// executable module; every other task applies to every module.
    pub fn tasks_for(plugins: &PluginRegistry, artifact_type: &ArtifactType) -> Vec<(Task, Step)> {
        plugins
            .plugins()
            .iter()
            .flat_map(|plugin: &Plugin| plugin.tasks().iter().cloned())
            .filter(|(task, _): &(Task, Step)| -> bool {
                artifact_type.is_executable() || task.name() != &TaskName::new("go-package")
            })
            .collect()
    }

    /// The `generate-go-work` task: regenerates a `go.work` covering every Go module in `graph`, via
    /// the real `go` toolchain (see [`Script::go_work`]). No declared or managed input of its own — the
    /// module directory list is embedded directly in the script's source, so a change to that set is
    /// already a change to the task's definition hash, without needing a `FileSetPattern` to detect it.
    /// Not bound to a lifecycle step: unlike the per-module tasks [`GoPlugin::tasks_for`] projects,
    /// this one is workspace-wide and runs once, before every module's tasks, via
    /// [`crate::executor::run_standalone_task`].
    pub fn generate_go_work_task(graph: &ModuleGraph, workspace_root: &WorkspaceRoot) -> Task {
        let module_directories: Vec<String> = graph
            .nodes()
            .iter()
            .filter(|node: &&ModuleNode| -> bool { matches!(node.module().language(), Language::Go) })
            .map(|node: &ModuleNode| -> String {
                let directory: AbsoluteDirectory = workspace_root
                    .to_absolute_directory()
                    .join_directory(node.identity().directory());
                // Every `AbsoluteDirectory` carries a trailing separator by construction (Sindri's own
                // directory convention), but `go work init`'s generated `use` entry does not match
                // against one — strip it here, in the string domain, since the typed `AbsoluteDirectory`
                // itself can no longer represent the slash-free form `go` needs.
                directory.to_string().trim_end_matches('/').to_string()
            })
            .collect();
        Task::new(
            TaskName::new("generate-go-work"),
            Script::go_work(&module_directories),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(["go.work"])),
            ParameterDeclarations::default(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_graph::ModuleGraph;
    use crate::module_tool::ModuleToolBinaries;
    use crate::nickel_import::ScriptResolutionState;
    use crate::parameter::ParameterName;
    use crate::parameter::ParameterState;
    use crate::parameter::ParameterValue;
    use crate::parameter::PluginName;
    use crate::runtime::DummyRuntime;
    use crate::script::Command;
    use crate::types::AbsoluteDirectory;
    use crate::types::BuildFile;
    use crate::types::RelativeDirectory;
    use crate::types::RelativeFile;
    use crate::types::WorkingDirectory;
    use crate::types::WorkspaceRoot;
    use crate::workspace::Workspace;
    use crate::workspace::WorkspaceConfig;
    use smol_str::SmolStr;
    use std::path::Path;
    use std::path::PathBuf;

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")))
    }

    fn plugin_registry() -> PluginRegistry {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .current_directory("/workspace")
            .build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        PluginRegistry::load(&workspace, &runtime).unwrap()
    }

    fn tasks_for(artifact_type: &ArtifactType) -> Vec<(Task, Step)> {
        GoPlugin::tasks_for(&plugin_registry(), artifact_type)
    }

    fn task_named<'tasks>(tasks: &'tasks [(Task, Step)], name: &str) -> &'tasks Task {
        tasks
            .iter()
            .find(|(task, _): &&(Task, Step)| -> bool { task.name() == &TaskName::new(name) })
            .map(|(task, _): &(Task, Step)| -> &Task { task })
            .expect("expected a task with this name")
    }

    fn find_task<'tasks>(tasks: &'tasks [(Task, Step)], name: &str) -> Option<&'tasks Task> {
        tasks
            .iter()
            .find(|(task, _): &&(Task, Step)| -> bool { task.name() == &TaskName::new(name) })
            .map(|(task, _): &(Task, Step)| -> &Task { task })
    }

    fn step_named<'tasks>(tasks: &'tasks [(Task, Step)], name: &str) -> &'tasks Step {
        tasks
            .iter()
            .find(|(task, _): &&(Task, Step)| -> bool { task.name() == &TaskName::new(name) })
            .map(|(_, step): &(Task, Step)| -> &Step { step })
            .expect("expected a task with this name")
    }

    #[test]
    fn tasks_for_binds_each_task_to_the_correct_step() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        assert_eq!(step_named(&tasks, "go-format"), &Step::new("format"));
        assert_eq!(step_named(&tasks, "go-compile"), &Step::new("compile"));
        assert_eq!(step_named(&tasks, "go-package"), &Step::new("package"));
        assert_eq!(step_named(&tasks, "go-test"), &Step::new("test"));
    }

    #[test]
    fn tasks_for_includes_go_package_only_for_an_executable_module() {
        let executable_tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        assert!(find_task(&executable_tasks, "go-package").is_some());
        let library_tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Library);
        assert!(find_task(&library_tasks, "go-package").is_none());
        // Every other task still applies to a library module.
        for name in ["go-format", "go-compile", "go-test"] {
            assert!(
                find_task(&library_tasks, name).is_some(),
                "expected {name} for a library module"
            );
        }
    }

    /// `mode` bound to `mode`, as `sindri-go`'s `go-compile`/`go-package` tasks declare it —
    /// `go-format`/`go-test` harmlessly ignore it (their declared parameter sets are empty), so this
    /// one binding fits every task [`resolve`] is called with.
    fn mode(mode: &str) -> ParameterState {
        ParameterState::new([(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterValue::new(format!("\"{mode}\"")),
        )])
    }

    fn resolve_with(task: &Task, parameter_state: &ParameterState) -> Vec<Command> {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        task.resolve(
            parameter_state,
            &RelativeDirectory::new_unchecked(""),
            &AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding")),
            &AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out")),
            &workspace_root,
            &ModuleToolBinaries::new(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap()
    }

    fn resolve(task: &Task) -> Vec<Command> {
        resolve_with(task, &mode("debug"))
    }

    #[test]
    fn go_package_writes_a_binary_to_the_output_directory() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        let commands: Vec<Command> = resolve(task_named(&tasks, "go-package"));
        assert_eq!(
            commands[0].arguments(),
            &[
                SmolStr::new("build"),
                SmolStr::new("-o"),
                SmolStr::new("/workspace/.target/out/"),
                SmolStr::new("./...")
            ]
        );
    }

    #[test]
    fn go_compile_never_writes_an_output_binary() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        let commands: Vec<Command> = resolve(task_named(&tasks, "go-compile"));
        assert_eq!(commands[0].arguments(), &[SmolStr::new("build"), SmolStr::new("./...")]);
    }

    #[test]
    fn release_mode_strips_symbols_and_trims_paths() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        let commands: Vec<Command> = resolve_with(task_named(&tasks, "go-package"), &mode("release"));
        assert_eq!(
            commands[0].arguments(),
            &[
                SmolStr::new("build"),
                SmolStr::new("-trimpath"),
                SmolStr::new("-ldflags"),
                SmolStr::new("-s -w"),
                SmolStr::new("-o"),
                SmolStr::new("/workspace/.target/out/"),
                SmolStr::new("./..."),
            ]
        );
    }

    #[test]
    fn go_format_and_go_test_declare_no_parameters() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        assert!(task_named(&tasks, "go-format").declared_parameters().is_empty());
        assert!(task_named(&tasks, "go-test").declared_parameters().is_empty());
    }

    #[test]
    fn go_compile_and_go_package_declare_the_mode_parameter() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        for name in ["go-compile", "go-package"] {
            let declared: Vec<(PluginName, ParameterName)> = task_named(&tasks, name)
                .declared_parameters()
                .iter()
                .map(|(plugin, name, _)| (plugin.clone(), name.clone()))
                .collect();
            assert_eq!(
                declared,
                vec![(PluginName::new("sindri-go"), ParameterName::new("mode"))],
                "{name} should declare the mode parameter"
            );
        }
    }

    #[test]
    fn go_compile_go_package_and_go_test_manage_go_work_as_an_input() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        for name in ["go-compile", "go-package", "go-test"] {
            assert_eq!(
                task_named(&tasks, name).managed_input().pattern().globs(),
                &[SmolStr::new("go.work")],
                "{name} should manage go.work as an input"
            );
        }
        // go-format never touches a module's dependency resolution, so it has no managed input.
        assert!(
            task_named(&tasks, "go-format")
                .managed_input()
                .pattern()
                .globs()
                .is_empty()
        );
    }

    #[test]
    fn go_compile_go_package_and_go_test_set_gowork_from_the_managed_input_directory() {
        let tasks: Vec<(Task, Step)> = tasks_for(&ArtifactType::Executable);
        for name in ["go-compile", "go-package", "go-test"] {
            let commands: Vec<Command> = resolve(task_named(&tasks, name));
            assert_eq!(
                commands[0].environment().get(&SmolStr::new("GOWORK")),
                Some(&SmolStr::new("/workspace/.target/generate-go-work/binding/go.work")),
                "{name} should point GOWORK at the generated workspace file"
            );
        }
    }

    /// A module graph whose entry `app` depends on a local `//lib/greeting` library, so the projected
    /// task covers more than just the workspace-root module.
    fn module_graph_with_dependency() -> ModuleGraph {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//lib/greeting" } ] } }"#,
            )
            .file(
                "/workspace/lib/greeting/sindri.build",
                r#"{ name = "greeting", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .build();
        let working_directory: WorkingDirectory =
            WorkingDirectory::derive(&workspace_root().to_absolute_directory(), &workspace_root());
        let config: WorkspaceConfig = WorkspaceConfig::load(&workspace_root(), &runtime).unwrap();
        let workspace: Workspace = Workspace::new(workspace_root(), working_directory, config);
        let entry: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        ModuleGraph::load(&entry, &workspace, &runtime).unwrap()
    }

    #[test]
    fn generate_go_work_task_declares_neither_input_and_outputs_go_work() {
        let task: Task = GoPlugin::generate_go_work_task(&module_graph_with_dependency(), &workspace_root());
        assert_eq!(task.name(), &TaskName::new("generate-go-work"));
        assert!(task.declared_input().pattern().globs().is_empty());
        assert!(task.managed_input().pattern().globs().is_empty());
        assert_eq!(task.output().pattern().globs(), &[SmolStr::new("go.work")]);
        assert!(task.declared_parameters().is_empty());
    }

    #[test]
    fn generate_go_work_task_inits_a_workspace_covering_every_go_module() {
        let task: Task = GoPlugin::generate_go_work_task(&module_graph_with_dependency(), &workspace_root());
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let commands: Vec<Command> = task
            .resolve(
                &ParameterState::default(),
                &RelativeDirectory::new_unchecked(""),
                &AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding")),
                &AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding")),
                &workspace_root(),
                &ModuleToolBinaries::new(),
                &mut resolution_state,
                &runtime,
            )
            .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program(), "rm");
        assert_eq!(commands[0].arguments(), &[SmolStr::new("-f"), SmolStr::new("go.work")]);
        assert_eq!(commands[1].program(), "go");
        assert_eq!(commands[1].arguments()[0], SmolStr::new("work"));
        assert_eq!(commands[1].arguments()[1], SmolStr::new("init"));
        let module_directories: &[SmolStr] = &commands[1].arguments()[2..];
        assert!(
            module_directories.contains(&SmolStr::new("/workspace")),
            "expected the entry module's directory, got {module_directories:?}"
        );
        assert!(
            module_directories.contains(&SmolStr::new("/workspace/lib/greeting")),
            "expected the dependency's directory, got {module_directories:?}"
        );
        // Both commands run inside the task's own output directory, relative to the workspace root —
        // not the module directory `go work init` would otherwise default to.
        for command in &commands {
            assert_eq!(
                command.working_directory().as_ref(),
                Path::new(".target/generate-go-work/binding")
            );
        }
    }
}
