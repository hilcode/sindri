use crate::error::SindriError;
use crate::executor::ExecutionConfig;
use crate::executor::TaskOutcome;
use crate::executor::execute_graph;
use crate::module_graph::ModuleGraph;
use crate::plugin::go_plugin;
use crate::plugin::{Plugin, Task};
use crate::runtime::Runtime;
use crate::telemetry::Telemetry;
use crate::types::AbsoluteDirectory;
use crate::types::BuildFile;
use crate::types::Qualifier;
#[cfg(test)]
use crate::types::Stdout;
use crate::types::Step;
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
        let plugin: Plugin = go_plugin();
        self.write(&[&plugin], show_all, &mut runtime.output())
    }

    pub fn run_compile(
        self,
        workspace: &Workspace,
        config: &ExecutionConfig,
        runtime: &impl Runtime,
    ) -> MietteResult<()> {
        let build_file: BuildFile = BuildFile::find(workspace, runtime)?;
        let module_graph: ModuleGraph = ModuleGraph::load(&build_file, workspace, runtime)?;
        for loaded_module in module_graph.modules() {
            runtime
                .log(&format!("Module loaded: {}", loaded_module.name().as_ref()))
                .map_err(|source| SindriError::Log { source })?;
        }
        let plugin: Plugin = go_plugin();
        let compile_step: Step = Step::new("compile");
        let graph: TaskGraph = self
            .build_task_graph(&[&plugin], &compile_step)
            .expect("compile is a built-in lifecycle step");
        let qualifier: Option<Qualifier> = build_file.qualifier();
        let absolute_working_directory: AbsoluteDirectory = workspace.absolute_working_directory();
        let absolute_build_directory: AbsoluteDirectory = workspace.absolute_build_directory();
        // A failing task makes `execute_graph` return early via `?`, so the telemetry write below is
        // skipped on a broken build. That is deliberate, not an oversight: a failed build is
        // diagnosed from its error, and dropping the partial trace keeps every telemetry.json a
        // record of a whole build rather than an aborted fragment.
        let outcomes: Vec<TaskOutcome> = execute_graph(
            &graph,
            &absolute_working_directory,
            workspace.workspace_root(),
            &absolute_build_directory,
            qualifier.as_ref(),
            config,
            runtime,
        )?;
        let _ = Telemetry::write(&outcomes, &absolute_build_directory, config.build_start(), runtime);
        Ok(())
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn write(&self, plugins: &[&Plugin], show_all: bool, writer: &mut impl Write) -> IoResult<()> {
        for step in &self.steps {
            let step_tasks: Vec<&Task> = plugins
                .iter()
                .flat_map(|plugin: &&Plugin| -> std::slice::Iter<'_, Task> { plugin.tasks().iter() })
                .filter(|task: &&Task| -> bool { task.step() == step })
                .collect();
            if step_tasks.is_empty() {
                if show_all {
                    writeln!(writer, "{step}")?;
                    writeln!(writer, "    (no tasks)")?;
                }
            } else {
                writeln!(writer, "{step}")?;
                for task in &step_tasks {
                    writeln!(writer, "    {:<20} {}", task.name(), task.command())?;
                }
            }
        }
        Ok(())
    }

    pub fn build_task_graph(&self, plugins: &[&Plugin], target: &Step) -> Option<TaskGraph> {
        let target_index: usize = self.steps.iter().position(|step: &Step| -> bool { step == target })?;
        let steps_in_scope: &[Step] = &self.steps[..=target_index];

        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        for step in steps_in_scope {
            for plugin in plugins {
                for task in plugin.tasks() {
                    if task.step() == step {
                        builder.add_node(TaskGraphNode {
                            task: task.clone(),
                            step: step.clone(),
                        });
                    }
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
    use crate::glob::GlobPatterns;
    use crate::plugin::{Plugin, PluginName, Task, TaskName, go_plugin};
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::types::BuildStart;
    use crate::types::Command;
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
    /// register the command outcomes (`gofmt`, `go build`) the test wants to exercise.
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
            .current_directory("/workspace")
    }

    fn make_task(name: &str, step: &str) -> Task {
        Task::new(
            TaskName::new(name),
            Step::new(step),
            Command::new("", [] as [&str; 0]),
            GlobPatterns::new(vec![], vec![]),
            GlobPatterns::new(vec![], vec![]),
        )
    }

    fn make_plugin(tasks: Vec<Task>) -> Plugin {
        Plugin::new(PluginName::new("test"), tasks)
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
        let plugin: Plugin = make_plugin(vec![
            make_task("compile-task", "compile"),
            make_task("test-task", "test"),
        ]);
        let lifecycle: Lifecycle = Lifecycle::new();
        let graph: TaskGraph = lifecycle.build_task_graph(&[&plugin], &Step::new("test")).unwrap();
        let names: Vec<&str> = graph
            .nodes()
            .iter()
            .map(|node: &TaskGraphNode| -> &str { node.task().name().as_ref() })
            .collect();
        // The executor derives step ordering positionally from this node order, so build_task_graph
        // must emit every compile-step task before every test-step task.
        assert_eq!(names, vec!["compile-task", "test-task"]);
    }

    #[test]
    fn write_hides_empty_steps_by_default() {
        let lifecycle: Lifecycle = Lifecycle::new();
        let plugin: Plugin = go_plugin();
        let mut buffer: Stdout = Stdout::default();
        lifecycle.write(&[&plugin], false, &mut buffer).unwrap();
        let output: &str = buffer.as_str();
        assert!(output.contains("go-compile"));
        assert!(output.contains("go-test"));
        assert!(!output.contains("(no tasks)"));
    }

    #[test]
    fn write_shows_empty_steps_with_all_flag() {
        let lifecycle: Lifecycle = Lifecycle::new();
        let plugin: Plugin = go_plugin();
        let mut buffer: Stdout = Stdout::default();
        lifecycle.write(&[&plugin], true, &mut buffer).unwrap();
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
            output.as_str().contains("go build"),
            "expected the go build task in the output, got:\n{}",
            output.as_str(),
        );
    }
}
