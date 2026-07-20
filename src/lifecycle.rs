use crate::error::SindriError;
use crate::executor::BuildContext;
use crate::executor::ExecutionConfig;
use crate::executor::ModuleLocation;
use crate::executor::ModuleRebuilt;
use crate::executor::TaskOutcome;
use crate::executor::execute_graph;
use crate::executor::run_standalone_task;
use crate::go_plugin::GoPlugin;
use crate::metadata_cache::MetadataCache;
use crate::module::ArtifactType;
use crate::module_graph::ModuleGraph;
use crate::runtime::Runtime;
use crate::task::Task;
use crate::telemetry::Telemetry;
use crate::types::AbsoluteDirectory;
use crate::types::BuildFile;
use crate::types::RelativeDirectory;
#[cfg(test)]
use crate::types::Stdout;
use crate::types::Step;
use crate::types::WorkspaceRoot;
use crate::workspace::Workspace;
use miette::Result as MietteResult;
use std::io::Result as IoResult;
use std::io::Write;

pub struct TaskGraphNode {
    task: Task,
    step: Step,
}

impl TaskGraphNode {
    pub fn new(task: Task, step: Step) -> TaskGraphNode {
        TaskGraphNode { task, step }
    }

    pub fn task(&self) -> &Task {
        &self.task
    }

    pub fn step(&self) -> &Step {
        &self.step
    }
}

pub struct TaskGraph {
    nodes: Vec<TaskGraphNode>,
}

impl TaskGraph {
    pub fn nodes(&self) -> &[TaskGraphNode] {
        &self.nodes
    }
}

pub struct TaskGraphBuilder {
    nodes: Vec<TaskGraphNode>,
}

impl TaskGraphBuilder {
    pub fn new() -> TaskGraphBuilder {
        TaskGraphBuilder { nodes: Vec::new() }
    }

    pub fn add_node(&mut self, node: TaskGraphNode) {
        self.nodes.push(node);
    }

    pub fn build(self) -> TaskGraph {
        TaskGraph { nodes: self.nodes }
    }
}

pub struct Lifecycle {
    steps: Vec<Step>,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            steps: vec![
                Step::new("start"),
                Step::new("generate"),
                Step::new("format"),
                Step::new("compile"),
                Step::new("document"),
                Step::new("test-compile"),
                Step::new("lint"),
                Step::new("test"),
                Step::new("integration-test"),
                Step::new("package"),
                Step::new("publish"),
                Step::new("end"),
            ],
        }
    }

    pub fn run_lifecycle(&self, show_all: bool, runtime: &impl Runtime) -> IoResult<()> {
        // The listing is workspace-generic, with no module in hand; show the executable form of the
        // go-compile task as a representative.
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        self.write(&tasks, show_all, &mut runtime.output())
    }

    pub fn run_compile(
        self,
        workspace: &Workspace,
        config: &ExecutionConfig,
        runtime: &impl Runtime,
    ) -> MietteResult<()> {
        let build_file: BuildFile = BuildFile::find(workspace, runtime)?;
        let module_graph: ModuleGraph = ModuleGraph::load(&build_file, workspace, runtime)?;
        for node in module_graph.nodes() {
            runtime
                .log(&format!("Module loaded: {}", node.module().name().as_ref()))
                .map_err(|source| SindriError::Log { source })?;
        }
        let compile_step: Step = Step::new("compile");
        let workspace_root: &WorkspaceRoot = workspace.workspace_root();
        let absolute_build_directory: AbsoluteDirectory = workspace.absolute_build_directory();
        let mut outcomes: Vec<TaskOutcome> = Vec::new();
        // Regenerate go.work — Go's local-dependency view, projected from the declared module
        // dependencies so it can never drift from them — before any module's tasks run, since
        // go-compile/go-test need it as a managed input. Run once, workspace-wide, through the same
        // resolve/dirtiness/run/persist sequence every other task goes through, so an unchanged module
        // set is a cache hit rather than a full regeneration on every build. It has no managed input of
        // its own, so the context supplied here is a placeholder — its real value (this task's own
        // output directory) is not known until it has run.
        let generate_go_work: Task = GoPlugin::generate_go_work_task(&module_graph, workspace_root);
        let cache_directory: AbsoluteDirectory =
            absolute_build_directory.join_directory(&RelativeDirectory::new(".metadata-cache"));
        let cache: MetadataCache = MetadataCache::new(cache_directory);
        let bootstrap_context: BuildContext = BuildContext::new(
            workspace_root,
            &absolute_build_directory,
            &absolute_build_directory,
            &cache,
        );
        let (go_work_outcome, go_work_output_directory): (TaskOutcome, AbsoluteDirectory) = run_standalone_task(
            &generate_go_work,
            &RelativeDirectory::new(""),
            &bootstrap_context,
            config,
            runtime,
        )?;
        outcomes.push(go_work_outcome);
        let context: BuildContext = BuildContext::new(
            workspace_root,
            &absolute_build_directory,
            &go_work_output_directory,
            &cache,
        );
        // Modules build in dependency-first order (the graph's node order), each in its own directory
        // and its own state subtree, so a dependency is fully built before anything that depends on
        // it. Each module compiles with the go-compile script its artifact type requires. A failing
        // task makes `execute_graph` return early via `?`, so a broken build writes no telemetry: the
        // partial trace is dropped, keeping every telemetry.json a whole-build record.
        let mut module_rebuilt: Vec<ModuleRebuilt> = Vec::with_capacity(module_graph.nodes().len());
        for node in module_graph.nodes() {
            let tasks: Vec<(Task, Step)> = GoPlugin::tasks(node.module().artifact_type());
            let graph: TaskGraph = self
                .build_task_graph(&tasks, &compile_step)
                .expect("compile is a built-in lifecycle step");
            let module_directory: AbsoluteDirectory = workspace_root
                .to_absolute_directory()
                .join_directory(node.identity().directory());
            let location: ModuleLocation = ModuleLocation::new(module_directory, node.identity().module_path());
            // A module must rebuild if any module it depends on rebuilt on this run. Dependency indices
            // are all smaller than this node's — nodes are stored dependency-first — so their rebuilt
            // status is already recorded.
            let dependency_rebuilt: ModuleRebuilt = ModuleRebuilt::new(
                node.dependencies()
                    .iter()
                    .any(|&index: &usize| module_rebuilt[index].is_rebuilt()),
            );
            let (module_outcomes, rebuilt): (Vec<TaskOutcome>, ModuleRebuilt) =
                execute_graph(&graph, &location, &context, dependency_rebuilt, config, runtime)?;
            module_rebuilt.push(rebuilt);
            outcomes.extend(module_outcomes);
        }
        let _ = Telemetry::write(&outcomes, &absolute_build_directory, config.build_start(), runtime);
        Ok(())
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    /// Print each lifecycle step and the tasks bound to it, one task name per line. A task's
    /// commands are not previewed here — they come from evaluating its `Script`, which needs real
    /// inputs (a workspace, a resolved file set) this purely informational listing does not have.
    pub fn write(&self, tasks: &[(Task, Step)], show_all: bool, writer: &mut impl Write) -> IoResult<()> {
        for step in &self.steps {
            let step_tasks: Vec<&Task> = tasks
                .iter()
                .filter(|(_, task_step): &&(Task, Step)| -> bool { task_step == step })
                .map(|(task, _): &(Task, Step)| -> &Task { task })
                .collect();
            if step_tasks.is_empty() {
                if show_all {
                    writeln!(writer, "{step}")?;
                    writeln!(writer, "    (no tasks)")?;
                }
            } else {
                writeln!(writer, "{step}")?;
                for task in &step_tasks {
                    writeln!(writer, "    {}", task.name())?;
                }
            }
        }
        Ok(())
    }

    pub fn build_task_graph(&self, tasks: &[(Task, Step)], target: &Step) -> Option<TaskGraph> {
        let target_index: usize = self.steps.iter().position(|step: &Step| -> bool { step == target })?;
        let steps_in_scope: &[Step] = &self.steps[..=target_index];

        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        for step in steps_in_scope {
            for (task, task_step) in tasks {
                if task_step == step {
                    builder.add_node(TaskGraphNode {
                        task: task.clone(),
                        step: step.clone(),
                    });
                }
            }
        }

        Some(builder.build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::Verbosity;
    use crate::file_set::FileSetPattern;
    use crate::parameter::ParameterDeclarations;
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::script::Script;
    use crate::task::DeclaredTaskInput;
    use crate::task::ManagedTaskInput;
    use crate::task::TaskName;
    use crate::task::TaskOutput;
    use crate::types::BuildStart;
    use crate::types::CommandOutput;
    use crate::types::Stderr;
    use crate::types::TaskStatus;
    use crate::workspace::Workspace;

    fn succeeded() -> CommandOutput {
        CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Succeeded)
    }

    fn failed() -> CommandOutput {
        CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Failed)
    }

    /// A runtime seeded with a minimal Go workspace and module, ready for a `compile` run. Callers
    /// register the command outcomes (`gofmt`, `go build`) the test wants to exercise; `generate-go-work`'s
    /// commands are stubbed here since every compile runs it.
    fn go_workspace() -> DummyRuntimeBuilder {
        DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .file(
                "/workspace/sindri.build",
                r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0" }"#,
            )
            .command("rm -f go.work", succeeded())
            .command("go work init", succeeded())
            .current_directory("/workspace")
    }

    /// Re-register a previous run's persisted files (task run records, the generated `go.work`) as
    /// real files in a fresh builder, so a replayed build observes them: a stub runtime records
    /// writes into a log its reads do not consult, so without this every task would look uncached on
    /// the replay.
    fn replay_with_state(mut builder: DummyRuntimeBuilder, previous: &DummyRuntime) -> DummyRuntimeBuilder {
        for (path, contents) in previous.written_files() {
            builder = builder.file(path, contents);
        }
        builder
    }

    fn make_task(name: &str) -> Task {
        Task::new(
            TaskName::new(name),
            Script::new("fun inputs => []"),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        )
    }

    #[test]
    fn lifecycle_contains_expected_steps_in_order() {
        let lifecycle: Lifecycle = Lifecycle::new();
        let names: Vec<&str> = lifecycle.steps().iter().map(|step| step.as_ref()).collect();
        assert_eq!(
            names,
            vec![
                "start",
                "generate",
                "format",
                "compile",
                "document",
                "test-compile",
                "lint",
                "test",
                "integration-test",
                "package",
                "publish",
                "end",
            ]
        );
    }

    #[test]
    fn nodes_are_ordered_by_lifecycle_step() {
        let tasks: Vec<(Task, Step)> = vec![
            (make_task("compile-task"), Step::new("compile")),
            (make_task("test-task"), Step::new("test")),
        ];
        let lifecycle: Lifecycle = Lifecycle::new();
        let graph: TaskGraph = lifecycle.build_task_graph(&tasks, &Step::new("test")).unwrap();
        let names: Vec<String> = graph
            .nodes()
            .iter()
            .map(|node: &TaskGraphNode| -> String { node.task().name().to_string() })
            .collect();
        // The executor derives step ordering positionally from this node order, so build_task_graph
        // must emit every compile-step task before every test-step task.
        assert_eq!(names, vec!["compile-task", "test-task"]);
    }

    #[test]
    fn write_hides_empty_steps_by_default() {
        let lifecycle: Lifecycle = Lifecycle::new();
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        let mut buffer: Stdout = Stdout::default();
        lifecycle.write(&tasks, false, &mut buffer).unwrap();
        let output: &str = buffer.as_str();
        assert!(output.contains("go-compile"));
        assert!(output.contains("go-test"));
        assert!(!output.contains("(no tasks)"));
    }

    #[test]
    fn write_shows_empty_steps_with_all_flag() {
        let lifecycle: Lifecycle = Lifecycle::new();
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        let mut buffer: Stdout = Stdout::default();
        lifecycle.write(&tasks, true, &mut buffer).unwrap();
        let output: &str = buffer.as_str();
        assert!(output.contains("(no tasks)"));
    }

    #[test]
    fn run_compile_logs_the_loaded_module() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .file(
                "/workspace/sindri.build",
                r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0" }"#,
            )
            .command("rm -f go.work", succeeded())
            .command("go work init", succeeded())
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .current_directory("/workspace")
            .build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Quiet, BuildStart::now());
        Lifecycle::new().run_compile(&workspace, &config, &runtime).unwrap();
        assert_eq!(runtime.logged(), vec!["Module loaded: my-app".to_string()]);
    }

    #[test]
    fn run_compile_builds_every_module_in_the_graph() {
        // The entry `app` depends on a local library `lib`. Both must be built, each into its own
        // state subtree — proving the scheduler runs the whole graph, not just the entry module.
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .file(
                "/workspace/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//lib" } ] } }"#,
            )
            .file(
                "/workspace/lib/sindri.build",
                r#"{ name = "lib", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .command("rm -f go.work", succeeded())
            .command("go work init", succeeded())
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .current_directory("/workspace")
            .build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Quiet, BuildStart::now());
        Lifecycle::new().run_compile(&workspace, &config, &runtime).unwrap();
        // The dependency is loaded and built before the entry.
        assert_eq!(
            runtime.logged(),
            vec!["Module loaded: lib".to_string(), "Module loaded: app".to_string()]
        );
        // Each module persisted its compile run record under its own subtree: the root module
        // directly under `.target`, the `lib` module under `.target/lib`.
        assert!(
            runtime
                .written_files()
                .iter()
                .any(|(path, _)| path.starts_with("/workspace/.target/go-compile")),
            "the entry module should have built and persisted a run record"
        );
        assert!(
            runtime
                .written_files()
                .iter()
                .any(|(path, _)| path.starts_with("/workspace/.target/lib/go-compile")),
            "the dependency module should have built and persisted a run record"
        );
        // generate-go-work ran (via the stubbed `go work init`) and persisted its own run record,
        // before either module's tasks — real go.work content, produced by the actual `go` toolchain,
        // is covered by the cli.rs integration tests rather than this stub run.
        assert!(
            runtime
                .written_files()
                .iter()
                .any(|(path, _)| path.starts_with("/workspace/.target/generate-go-work")),
            "generate-go-work should have run and persisted a run record"
        );
    }

    /// A two-module workspace: an `app` executable in `/workspace/app` depending on a local `//lib`
    /// library in the sibling `/workspace/lib`. The directories are disjoint, so neither module's
    /// source glob sweeps the other — the dependency edge is the only thing that can tie `app`'s
    /// freshness to `lib`'s. `ModuleRebuilt` propagation does not depend on `go.work`/`GOWORK` (that
    /// wiring is a later phase) — a `DummyRuntime` stub matches a command by its text alone, so
    /// whether the real environment resolves a sibling import is irrelevant here.
    fn multi_module_workspace() -> DummyRuntimeBuilder {
        DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .file(
                "/workspace/app/sindri.build",
                r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
                     dependencies = { compile = [ { module = "//lib" } ] } }"#,
            )
            .file("/workspace/app/main.go", "package main\n\nfunc main() {}\n")
            .file(
                "/workspace/lib/sindri.build",
                r#"{ name = "lib", language = "go", type = "library", version = "0.1.0" }"#,
            )
            .file("/workspace/lib/lib.go", "package lib\n")
            .command("rm -f go.work", succeeded())
            .command("go work init", succeeded())
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .current_directory("/workspace/app")
    }

    #[test]
    fn editing_a_dependency_forces_the_dependent_via_the_edge() {
        // A first build persists every module's run record.
        let seed: DummyRuntime = multi_module_workspace().build();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Quiet, BuildStart::now());
        Lifecycle::new()
            .run_compile(&Workspace::locate(&seed).unwrap(), &config, &seed)
            .unwrap();
        // Replay with every module's state seeded — so an unchanged module is a cache hit — but the
        // dependency's source edited, so `lib` rebuilds and, through the edge, must force `app`.
        let rebuild: DummyRuntime = replay_with_state(multi_module_workspace(), &seed)
            .file("/workspace/lib/lib.go", "package lib\n\nvar Changed = true\n")
            .build();
        Lifecycle::new()
            .run_compile(&Workspace::locate(&rebuild).unwrap(), &config, &rebuild)
            .unwrap();
        assert!(
            rebuild
                .written_files()
                .iter()
                .any(|(path, _)| path.starts_with("/workspace/.target/lib/go-compile")),
            "the edited dependency should rebuild"
        );
        assert!(
            rebuild
                .written_files()
                .iter()
                .any(|(path, _)| path.starts_with("/workspace/.target/app/go-compile")),
            "the dependent should rebuild via the edge even though its own source is unchanged"
        );
    }

    #[test]
    fn an_unchanged_multi_module_tree_rebuilds_nothing() {
        let seed: DummyRuntime = go_workspace()
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .build();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Normal, BuildStart::now());
        Lifecycle::new()
            .run_compile(&Workspace::locate(&seed).unwrap(), &config, &seed)
            .unwrap();
        // Replay with the module's run record seeded and nothing changed: no task re-runs, so none
        // re-persists a record, and a build that ran no tasks prints nothing.
        let replay: DummyRuntime = replay_with_state(
            go_workspace()
                .command("gofmt -l .", succeeded())
                .command("go build", succeeded()),
            &seed,
        )
        .build();
        Lifecycle::new()
            .run_compile(&Workspace::locate(&replay).unwrap(), &config, &replay)
            .unwrap();
        assert!(
            replay.captured_output().is_empty(),
            "an unchanged tree should be silent; got: {:?}",
            replay.captured_output().as_str()
        );
    }

    #[test]
    fn an_unchanged_go_module_set_leaves_generate_go_work_a_cache_hit() {
        fn generate_go_work_run_records(runtime: &DummyRuntime) -> usize {
            runtime
                .written_files()
                .iter()
                .filter(|(path, _)| path.starts_with("/workspace/.target/generate-go-work"))
                .count()
        }

        let seed: DummyRuntime = go_workspace()
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .build();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Normal, BuildStart::now());
        Lifecycle::new()
            .run_compile(&Workspace::locate(&seed).unwrap(), &config, &seed)
            .unwrap();
        assert_eq!(
            generate_go_work_run_records(&seed),
            1,
            "the first build should persist generate-go-work's run record"
        );

        // Replay with the module set unchanged: generate-go-work's script embeds the module directory
        // list directly in its source, so an unchanged set means an unchanged definition hash — a
        // cache hit, with no re-run and no fresh record.
        let replay: DummyRuntime = replay_with_state(
            go_workspace()
                .command("gofmt -l .", succeeded())
                .command("go build", succeeded()),
            &seed,
        )
        .build();
        Lifecycle::new()
            .run_compile(&Workspace::locate(&replay).unwrap(), &config, &replay)
            .unwrap();
        assert_eq!(
            generate_go_work_run_records(&replay),
            0,
            "an unchanged module set should leave generate-go-work a cache hit"
        );
        assert!(
            !replay.captured_output().as_str().contains("generate-go-work"),
            "a cache-hit generate-go-work should print no progress line; got: {:?}",
            replay.captured_output().as_str()
        );
    }

    #[test]
    fn run_compile_creates_the_build_directory_and_writes_telemetry() {
        let runtime: DummyRuntime = go_workspace()
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Quiet, BuildStart::now());
        Lifecycle::new().run_compile(&workspace, &config, &runtime).unwrap();
        assert!(runtime.created_directory("/workspace/.target"));
        assert!(
            runtime.written_file("/workspace/.target/telemetry.json").is_some(),
            "expected a telemetry trace to be written to the build directory",
        );
    }

    #[test]
    fn run_compile_fails_when_the_build_command_fails() {
        let runtime: DummyRuntime = go_workspace()
            .command("gofmt -l .", succeeded())
            .command("go build", failed())
            .build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Quiet, BuildStart::now());
        let result: MietteResult<()> = Lifecycle::new().run_compile(&workspace, &config, &runtime);
        assert!(result.is_err(), "a failing build command should fail the compile");
    }

    #[test]
    fn run_compile_fails_when_no_build_file_is_present() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .current_directory("/workspace")
            .build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        let config: ExecutionConfig = ExecutionConfig::new(Verbosity::Quiet, BuildStart::now());
        let result: MietteResult<()> = Lifecycle::new().run_compile(&workspace, &config, &runtime);
        assert!(result.is_err(), "compile should fail when no build file can be found");
    }

    #[test]
    fn run_lifecycle_prints_the_compile_step_and_its_tasks() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        Lifecycle::new().run_lifecycle(false, &runtime).unwrap();
        let output: Stdout = runtime.captured_output();
        assert!(
            output.as_str().contains("compile"),
            "expected the compile step in the output, got:\n{}",
            output.as_str(),
        );
        assert!(
            output.as_str().contains("go-compile"),
            "expected the go-compile task in the output, got:\n{}",
            output.as_str(),
        );
    }
}
