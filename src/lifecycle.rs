use crate::module;
use crate::module::Module;
use crate::output::Output;
use crate::plugin::go_plugin;
use crate::plugin::{Plugin, Task};
use crate::types::BuildFile;
use crate::types::Step;
use crate::types::WorkspaceRoot;
use ::std::path::Path;
use miette::IntoDiagnostic;
use std::io::{self, Write};

pub struct TaskGraphNode {
    pub task: Task,
    pub step: Step,
}

pub struct TaskGraph {
    pub nodes: Vec<TaskGraphNode>,
    pub edges: Vec<(usize, usize)>,
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

    pub fn run_lifecycle(&self, show_all: bool, output: &mut impl Output) -> io::Result<()> {
        let plugin: Plugin = go_plugin();
        self.write(&[&plugin], show_all, output)
    }

    pub fn run_compile(
        self,
        current_directory: &Path,
        workspace_root: &WorkspaceRoot,
        output: &mut impl Output,
    ) -> miette::Result<()> {
        let build_file: BuildFile = module::find_entry_point(current_directory, workspace_root)?;
        let loaded_module: Module = module::load(&build_file)?;
        output.info(&format!("Module loaded: {}", loaded_module.name.as_ref()));
        let plugin: Plugin = go_plugin();
        let compile_step: Step = Step::new("compile");
        let graph: TaskGraph = self
            .build_task_graph(&[&plugin], &compile_step)
            .expect("compile is a built-in lifecycle step");
        for node in &graph.nodes {
            writeln!(
                output,
                "  {:<20} {:<14} {}",
                node.task.name, node.step, node.task.command
            )
            .into_diagnostic()?;
        }
        Ok(())
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn write(&self, plugins: &[&Plugin], show_all: bool, writer: &mut impl Write) -> io::Result<()> {
        for step in &self.steps {
            let step_tasks: Vec<&Task> = plugins
                .iter()
                .flat_map(|plugin| plugin.tasks.iter())
                .filter(|task| &task.step == step)
                .collect();
            if step_tasks.is_empty() {
                if show_all {
                    writeln!(writer, "{step}")?;
                    writeln!(writer, "    (no tasks)")?;
                }
            } else {
                writeln!(writer, "{step}")?;
                for task in &step_tasks {
                    writeln!(writer, "    {:<20} {}", task.name, task.command)?;
                }
            }
        }
        Ok(())
    }

    pub fn build_task_graph(&self, plugins: &[&Plugin], target: &Step) -> Option<TaskGraph> {
        let target_index: usize = self.steps.iter().position(|step| step == target)?;
        let steps_in_scope: &[Step] = &self.steps[..=target_index];

        let mut nodes: Vec<TaskGraphNode> = Vec::new();
        for step in steps_in_scope {
            for plugin in plugins {
                for task in &plugin.tasks {
                    if &task.step == step {
                        nodes.push(TaskGraphNode {
                            task: task.clone(),
                            step: step.clone(),
                        });
                    }
                }
            }
        }

        let mut edges: Vec<(usize, usize)> = Vec::new();
        let node_indices_by_occupied_step: Vec<Vec<usize>> = steps_in_scope
            .iter()
            .map(|step: &Step| {
                nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| &node.step == step)
                    .map(|(node_index, _)| node_index)
                    .collect::<Vec<usize>>()
            })
            .filter(|node_indices: &Vec<usize>| !node_indices.is_empty())
            .collect();
        for window in node_indices_by_occupied_step.windows(2) {
            let from_indices: &Vec<usize> = &window[0];
            let to_indices: &Vec<usize> = &window[1];
            for &from_index in from_indices {
                for &to_index in to_indices {
                    edges.push((from_index, to_index));
                }
            }
        }

        Some(TaskGraph { nodes, edges })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::CapturedOutput;
    use crate::plugin::{Plugin, PluginName, ShellCommand, Task, TaskName, go_plugin};
    use crate::types::WorkspaceRoot;
    use std::fs;
    use tempfile::TempDir;

    fn make_task(name: &str, step: &str) -> Task {
        Task {
            name: TaskName::new(name),
            step: Step::new(step),
            command: ShellCommand::new(""),
            inputs: vec![],
            outputs: vec![],
        }
    }

    fn make_plugin(tasks: Vec<Task>) -> Plugin {
        Plugin {
            name: PluginName::new("test"),
            tasks,
        }
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
    fn tasks_within_same_step_have_no_edges() {
        let plugin: Plugin = make_plugin(vec![make_task("task-a", "compile"), make_task("task-b", "compile")]);
        let lifecycle: Lifecycle = Lifecycle::new();
        let graph: TaskGraph = lifecycle.build_task_graph(&[&plugin], &Step::new("compile")).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn all_tasks_in_earlier_step_precede_later_step_tasks() {
        let plugin: Plugin = make_plugin(vec![
            make_task("compile-task", "compile"),
            make_task("test-task", "test"),
        ]);
        let lifecycle: Lifecycle = Lifecycle::new();
        let graph: TaskGraph = lifecycle.build_task_graph(&[&plugin], &Step::new("test")).unwrap();
        assert_eq!(graph.nodes.len(), 2);
        let compile_index: usize = graph
            .nodes
            .iter()
            .position(|node| node.task.name.as_ref() == "compile-task")
            .unwrap();
        let test_index: usize = graph
            .nodes
            .iter()
            .position(|node| node.task.name.as_ref() == "test-task")
            .unwrap();
        assert!(graph.edges.contains(&(compile_index, test_index)));
        assert!(!graph.edges.contains(&(test_index, compile_index)));
    }

    #[test]
    fn write_hides_empty_steps_by_default() {
        let lifecycle: Lifecycle = Lifecycle::new();
        let plugin: Plugin = go_plugin();
        let mut buffer: Vec<u8> = Vec::new();
        lifecycle.write(&[&plugin], false, &mut buffer).unwrap();
        let output: String = String::from_utf8(buffer).unwrap();
        assert!(output.contains("go-compile"));
        assert!(output.contains("go-test"));
        assert!(!output.contains("(no tasks)"));
    }

    #[test]
    fn write_shows_empty_steps_with_all_flag() {
        let lifecycle: Lifecycle = Lifecycle::new();
        let plugin: Plugin = go_plugin();
        let mut buffer: Vec<u8> = Vec::new();
        lifecycle.write(&[&plugin], true, &mut buffer).unwrap();
        let output: String = String::from_utf8(buffer).unwrap();
        assert!(output.contains("(no tasks)"));
    }

    #[test]
    fn run_compile_logs_module_name_and_writes_task_graph() {
        let directory: TempDir = TempDir::new().unwrap();
        fs::write(
            directory.path().join("sindri.workspace"),
            r#"{ name = "test", sindri_version = "0.1.0" }"#,
        )
        .unwrap();
        fs::write(
            directory.path().join("sindri.build"),
            r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0" }"#,
        )
        .unwrap();
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(directory.path().to_path_buf());
        let lifecycle: Lifecycle = Lifecycle::new();
        let mut output: CapturedOutput = CapturedOutput::new();
        lifecycle
            .run_compile(directory.path(), &workspace_root, &mut output)
            .unwrap();
        assert!(
            output.log.iter().any(|message| message.contains("my-app")),
            "module name should be logged; got: {:?}",
            output.log
        );
        assert!(
            output.stdout_str().contains("go-compile"),
            "task graph should include go-compile"
        );
    }
}
