use crate::file_set::FileSetPattern;
use crate::module::ArtifactType;
use crate::module_graph::ModuleGraph;
use crate::module_graph::ModuleNode;
use crate::parameter::Parameter;
use crate::parameter::ParameterDeclarations;
use crate::parameter::ParameterName;
use crate::parameter::ParameterType;
use crate::parameter::PluginName;
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

/// A platform-independent superset of every file type the Go toolchain might compile — all Go and
/// C-family sources cgo can pull in, plus assembly and the module manifests. Over-inclusion only
/// ever costs a spurious rebuild; it never misses an input. Per-target precision via `go list` is
/// future work.
fn go_source_superset() -> FileSetPattern {
    FileSetPattern::new(["**/*.{go,c,h,cc,cpp,cxx,hh,hpp,hxx,m,s,S}", "go.mod", "go.sum"])
}

/// The built-in Go plugin's tasks, contributed as shipped Nickel `Script`s rather than a hand-built
/// `Command`.
pub struct GoPlugin;

impl GoPlugin {
    /// `go-compile`'s declared parameters: `mode` selects between a debug build (the Go toolchain's
    /// own defaults, kept while iterating) and a release build (`-trimpath -ldflags "-s -w"`, stripping
    /// symbols and embedded build paths for a distributable binary). Every module built with `go-compile`
    /// must bind it — there is no implicit default — so a build file that never sets it fails with a
    /// named `ParameterMissing` error rather than silently picking one mode over the other.
    fn compile_parameters() -> ParameterDeclarations {
        ParameterDeclarations::new([Parameter::new(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterType::new(r#"std.contract.from_predicate (fun value => value == "debug" || value == "release")"#),
        )])
    }

    /// The `go-format` / `go-compile` / `go-test` tasks, each paired with the lifecycle step it is
    /// bound to. `go-compile`'s script depends on `artifact_type`: an executable links a runnable
    /// binary into the tracked output directory, so its build is `go build -o <output-directory>/
    /// ./...` and the binary becomes a tracked output; a library has no runnable binary — `go build`
    /// on it only type-checks and compiles its packages into Go's own cache — so it is
    /// `go build ./...`, which also avoids the `-o` form's "no main packages to build" error on a
    /// module with no executable. `go-compile` and `go-test` both manage a `go.work` input: the file
    /// [`GoPlugin::generate_go_work_task`] produces, so a module set change (which changes that task's
    /// output) invalidates every module's compile and test the same way an edited source file would.
    pub fn tasks(artifact_type: &ArtifactType) -> Vec<(Task, Step)> {
        let compile_script: Script = match artifact_type {
            ArtifactType::Executable => Script::go_compile_executable(),
            _ => Script::go_compile(),
        };
        vec![
            (
                Task::new(
                    TaskName::new("go-format"),
                    Script::go_format(),
                    DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
                    ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                    TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
                    ParameterDeclarations::default(),
                ),
                Step::new("format"),
            ),
            (
                Task::new(
                    TaskName::new("go-compile"),
                    compile_script,
                    DeclaredTaskInput::new(go_source_superset()),
                    ManagedTaskInput::new(FileSetPattern::new(["go.work"])),
                    TaskOutput::new(FileSetPattern::new(["**/*"])),
                    GoPlugin::compile_parameters(),
                ),
                Step::new("compile"),
            ),
            (
                Task::new(
                    TaskName::new("go-test"),
                    Script::go_test(),
                    DeclaredTaskInput::new(go_source_superset()),
                    ManagedTaskInput::new(FileSetPattern::new(["go.work"])),
                    TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
                    ParameterDeclarations::default(),
                ),
                Step::new("test"),
            ),
        ]
    }

    /// The `generate-go-work` task: regenerates a `go.work` covering every Go module in `graph`, via
    /// the real `go` toolchain (see [`Script::go_work`]). No declared or managed input of its own — the
    /// module directory list is embedded directly in the script's source, so a change to that set is
    /// already a change to the task's definition hash, without needing a `FileSetPattern` to detect it.
    /// Not bound to a lifecycle step: unlike the per-module tasks `GoPlugin::tasks` returns, this one
    /// is workspace-wide and runs once, before every module's tasks, via
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

    fn task_named<'tasks>(tasks: &'tasks [(Task, Step)], name: &str) -> &'tasks Task {
        tasks
            .iter()
            .find(|(task, _): &&(Task, Step)| -> bool { task.name() == &TaskName::new(name) })
            .map(|(task, _): &(Task, Step)| -> &Task { task })
            .expect("expected a task with this name")
    }

    fn step_named<'tasks>(tasks: &'tasks [(Task, Step)], name: &str) -> &'tasks Step {
        tasks
            .iter()
            .find(|(task, _): &&(Task, Step)| -> bool { task.name() == &TaskName::new(name) })
            .map(|(_, step): &(Task, Step)| -> &Step { step })
            .expect("expected a task with this name")
    }

    #[test]
    fn go_plugin_binds_each_task_to_the_correct_step() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        assert_eq!(step_named(&tasks, "go-format"), &Step::new("format"));
        assert_eq!(step_named(&tasks, "go-compile"), &Step::new("compile"));
        assert_eq!(step_named(&tasks, "go-test"), &Step::new("test"));
    }

    /// `mode` bound to `mode`, as `sindri-go`'s `go-compile` task now declares it — `go-test`
    /// harmlessly ignores it (its declared parameter set is empty), so this one binding fits every
    /// task [`resolve`] is called with.
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
    fn executable_go_compile_writes_a_binary_to_the_output_directory() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        let commands: Vec<Command> = resolve(task_named(&tasks, "go-compile"));
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
    fn library_go_compile_omits_the_output_binary() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Library);
        let commands: Vec<Command> = resolve(task_named(&tasks, "go-compile"));
        assert_eq!(commands[0].arguments(), &[SmolStr::new("build"), SmolStr::new("./...")]);
    }

    #[test]
    fn release_mode_strips_symbols_and_trims_paths() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Library);
        let commands: Vec<Command> = resolve_with(task_named(&tasks, "go-compile"), &mode("release"));
        assert_eq!(
            commands[0].arguments(),
            &[
                SmolStr::new("build"),
                SmolStr::new("-trimpath"),
                SmolStr::new("-ldflags"),
                SmolStr::new("-s -w"),
                SmolStr::new("./..."),
            ]
        );
    }

    #[test]
    fn go_format_and_go_test_declare_no_parameters() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        assert!(task_named(&tasks, "go-format").declared_parameters().is_empty());
        assert!(task_named(&tasks, "go-test").declared_parameters().is_empty());
    }

    #[test]
    fn go_compile_declares_the_mode_parameter() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        let declared: Vec<(PluginName, ParameterName)> = task_named(&tasks, "go-compile")
            .declared_parameters()
            .iter()
            .map(|(plugin, name, _)| (plugin.clone(), name.clone()))
            .collect();
        assert_eq!(
            declared,
            vec![(PluginName::new("sindri-go"), ParameterName::new("mode"))]
        );
    }

    #[test]
    fn go_compile_and_go_test_manage_go_work_as_an_input() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        assert_eq!(
            task_named(&tasks, "go-compile").managed_input().pattern().globs(),
            &[SmolStr::new("go.work")]
        );
        assert_eq!(
            task_named(&tasks, "go-test").managed_input().pattern().globs(),
            &[SmolStr::new("go.work")]
        );
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
    fn go_compile_and_go_test_set_gowork_from_the_managed_input_directory() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        for name in ["go-compile", "go-test"] {
            let commands: Vec<Command> = resolve(task_named(&tasks, name));
            assert_eq!(
                commands[0].environment().get(&SmolStr::new("GOWORK")),
                Some(&SmolStr::new("/workspace/.target/generate-go-work/binding/go.work")),
                "{name} should point GOWORK at the generated workspace file"
            );
        }
    }

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")))
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
