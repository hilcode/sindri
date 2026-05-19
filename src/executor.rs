use crate::error::SindriError;
use crate::lifecycle::TaskGraph;
use crate::lifecycle::TaskGraphNode;
use crate::plugin::Task;
use crate::runtime::Runtime;
use crate::types::AbsoluteDirectory;
use crate::types::BuildStart;
use crate::types::CommandOutput;
use crate::types::Fiber;
use crate::types::Stdout;
use crate::types::Step;
use crate::types::TaskGraphNodeId;
use crate::types::TaskStart;
use crate::types::TaskStatus;
use miette::IntoDiagnostic;
use miette::Result as MietteResult;
use std::io::Result as IoResult;
use std::io::Write;
use std::thread::ScopedJoinHandle;
use std::time::Duration;

#[derive(Debug, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

pub struct ExecutionConfig {
    verbosity: Verbosity,
    build_start: BuildStart,
}

impl ExecutionConfig {
    pub fn new(verbosity: Verbosity, build_start: BuildStart) -> ExecutionConfig {
        ExecutionConfig { verbosity, build_start }
    }

    pub fn build_start(&self) -> BuildStart {
        self.build_start
    }
}

#[derive(Debug)]
pub struct TaskOutcome {
    task: Task,
    output: CommandOutput,
    task_duration: Duration,
    task_start: TaskStart,
    fiber: Fiber,
}

impl TaskOutcome {
    pub fn from(task: Task, result: IoResult<CommandOutput>, task_start: TaskStart, fiber: Fiber) -> TaskOutcome {
        let output: CommandOutput = result.unwrap_or_else(|error| {
            CommandOutput::new(
                Stdout::default(),
                error.to_string().into_bytes().into(),
                TaskStatus::Failed,
            )
        });
        TaskOutcome {
            task,
            output,
            task_duration: task_start.elapsed(),
            task_start,
            fiber,
        }
    }

    #[cfg(test)]
    pub fn new(
        task: Task,
        output: CommandOutput,
        task_duration: Duration,
        task_start: TaskStart,
        fiber: Fiber,
    ) -> TaskOutcome {
        TaskOutcome {
            task,
            output,
            task_duration,
            task_start,
            fiber,
        }
    }

    pub fn task(&self) -> &Task {
        &self.task
    }

    pub fn output(&self) -> &CommandOutput {
        &self.output
    }

    pub fn task_duration(&self) -> Duration {
        self.task_duration
    }

    pub fn task_start(&self) -> TaskStart {
        self.task_start
    }

    pub fn fiber(&self) -> Fiber {
        self.fiber
    }
}

fn format_duration(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

fn group_nodes_by_step(nodes: &[TaskGraphNode]) -> Vec<Vec<TaskGraphNodeId>> {
    let mut groups: Vec<Vec<TaskGraphNodeId>> = Vec::new();
    let mut current_step: Option<&Step> = None;
    for (index, node) in nodes.iter().enumerate() {
        if current_step.is_none_or(|step: &Step| -> bool { step != node.step() }) {
            groups.push(Vec::new());
            current_step = Some(node.step());
        }
        groups.last_mut().unwrap().push(TaskGraphNodeId::new(index));
    }
    groups
}

fn run_step(
    node_ids: &[TaskGraphNodeId],
    nodes: &[TaskGraphNode],
    working_directory: &AbsoluteDirectory,
    runtime: &impl Runtime,
) -> Vec<TaskOutcome> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = node_ids
            .iter()
            .enumerate()
            .map(|(index, &node_id)| {
                let fiber: Fiber = Fiber::new(index);
                let task: Task = nodes[node_id.value()].task().clone();
                scope.spawn(move || -> TaskOutcome {
                    let task_start: TaskStart = TaskStart::new(runtime.now());
                    let result: IoResult<CommandOutput> =
                        runtime.run_command(task.command(), working_directory.as_ref());
                    TaskOutcome::from(task, result, task_start, fiber)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle: ScopedJoinHandle<'_, TaskOutcome>| -> TaskOutcome { handle.join().unwrap() })
            .collect()
    })
}

pub fn execute_graph(
    graph: &TaskGraph,
    working_directory: &AbsoluteDirectory,
    config: &ExecutionConfig,
    runtime: &impl Runtime,
) -> MietteResult<Vec<TaskOutcome>> {
    let step_groups: Vec<Vec<TaskGraphNodeId>> = group_nodes_by_step(graph.nodes());
    let mut all_outcomes: Vec<TaskOutcome> = Vec::new();
    for group in &step_groups {
        if config.verbosity != Verbosity::Quiet {
            for &node_id in group {
                writeln!(
                    runtime.output(),
                    "  \u{2192} {}",
                    graph.nodes()[node_id.value()].task().name()
                )
                .into_diagnostic()?;
            }
        }
        let step_outcomes: Vec<TaskOutcome> = run_step(group, graph.nodes(), working_directory, runtime);
        let batch_start: usize = all_outcomes.len();
        all_outcomes.extend(step_outcomes);
        let batch: &[TaskOutcome] = &all_outcomes[batch_start..];
        let mut failure_index: Option<usize> = None;
        for (local_index, outcome) in batch.iter().enumerate() {
            if config.verbosity != Verbosity::Quiet {
                let symbol: &str = if outcome.output.status().is_success() {
                    "\u{2713}"
                } else {
                    "\u{2717}"
                };
                writeln!(
                    runtime.output(),
                    "  {} {} ({})",
                    symbol,
                    outcome.task.name(),
                    format_duration(outcome.task_duration)
                )
                .into_diagnostic()?;
            }
            if outcome.output.status().is_success() && config.verbosity == Verbosity::Verbose {
                let combined: String = outcome.output.combined_output();
                if !combined.is_empty() {
                    writeln!(runtime.output(), "{combined}").into_diagnostic()?;
                }
            }
            if !outcome.output.status().is_success() && failure_index.is_none() {
                failure_index = Some(batch_start + local_index);
            }
        }
        if let Some(failure_idx) = failure_index {
            let failed: &TaskOutcome = &all_outcomes[failure_idx];
            return Err(SindriError::TaskFailed {
                task_name: failed.task.name().clone(),
                command: failed.task.command().clone(),
                output: failed.output.combined_output(),
            }
            .into());
        }
    }
    Ok(all_outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle::TaskGraph;
    use crate::lifecycle::TaskGraphBuilder;
    use crate::lifecycle::TaskGraphNode;
    use crate::plugin::Task;
    use crate::plugin::TaskName;
    use crate::runtime::Bootstrap;
    use crate::runtime::DummyRuntime;
    use crate::runtime::SystemFileSystem;
    use crate::types::ShellCommand;
    use crate::types::Stderr;
    use crate::types::Step;
    use std::path::PathBuf;
    use std::time::Instant;

    fn system_runtime() -> impl Runtime {
        SystemFileSystem.into_runtime(BuildStart::now(), None).unwrap()
    }

    fn make_graph(tasks: Vec<(&str, &str, &str)>) -> TaskGraph {
        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        for (name, step, command) in tasks {
            builder.add_node(TaskGraphNode::new(
                Task::new(
                    TaskName::new(name),
                    Step::new(step),
                    ShellCommand::new(command),
                    vec![],
                    vec![],
                ),
                Step::new(step),
            ));
        }
        builder.build()
    }

    fn working_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(std::env::temp_dir())
    }

    fn workspace_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace"))
    }

    fn default_config() -> ExecutionConfig {
        ExecutionConfig {
            verbosity: Verbosity::Normal,
            build_start: BuildStart::now(),
        }
    }

    fn config(verbosity: Verbosity) -> ExecutionConfig {
        ExecutionConfig {
            verbosity,
            build_start: BuildStart::now(),
        }
    }

    fn succeeded(stdout: &str) -> CommandOutput {
        CommandOutput::new(
            Stdout::new(stdout.as_bytes().to_vec()),
            Stderr::default(),
            TaskStatus::Succeeded,
        )
    }

    fn failed(stderr: &str) -> CommandOutput {
        CommandOutput::new(
            Stdout::default(),
            Stderr::new(stderr.as_bytes().to_vec()),
            TaskStatus::Failed,
        )
    }

    // A real command is executed here to keep coverage of actual process spawning and stdout capture.
    #[test]
    fn succeeding_command_exits_zero_and_captures_stdout() {
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo hello")]);
        let outcomes: Vec<TaskOutcome> =
            execute_graph(&graph, &working_directory(), &default_config(), &system_runtime()).unwrap();
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].output().status(), TaskStatus::Succeeded);
        assert_eq!(outcomes[0].output().stdout().as_bytes().trim_ascii_end(), b"hello");
    }

    #[test]
    fn failing_command_exits_nonzero() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        let result: MietteResult<Vec<TaskOutcome>> =
            execute_graph(&graph, &workspace_directory(), &default_config(), &runtime);
        assert!(result.is_err());
    }

    #[test]
    fn task_failure_error_contains_command() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        let error: miette::Error =
            execute_graph(&graph, &workspace_directory(), &default_config(), &runtime).unwrap_err();
        assert!(
            error.to_string().contains("false"),
            "error message should contain the command; got: {error}"
        );
    }

    #[test]
    fn quiet_suppresses_progress_lines_on_success() {
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo hello")]);
        let runtime: DummyRuntime = DummyRuntime::builder()
            .command("echo hello", succeeded("hello\n"))
            .build();
        execute_graph(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime).unwrap();
        assert!(
            runtime.captured_output().is_empty(),
            "expected no output with quiet; got: {:?}",
            runtime.captured_output()
        );
    }

    #[test]
    fn quiet_still_returns_error_on_failure() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        assert!(
            execute_graph(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime).is_err(),
            "expected an error even with quiet verbosity"
        );
    }

    #[test]
    fn failing_task_output_shown_in_error_regardless_of_verbosity() {
        let graph: TaskGraph = make_graph(vec![("ls-task", "compile", "ls /sindri_test_nonexistent")]);
        let runtime: DummyRuntime = DummyRuntime::builder()
            .command("ls /sindri_test_nonexistent", failed("No such file or directory"))
            .build();
        let error: miette::Error =
            execute_graph(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime).unwrap_err();
        assert!(
            error.to_string().contains("No such file or directory"),
            "failing task output should be in the error even with quiet verbosity; got: {error}"
        );
        assert!(
            runtime.captured_output().is_empty(),
            "quiet verbosity should suppress progress output; got: {:?}",
            runtime.captured_output()
        );
    }

    #[test]
    fn verbose_shows_stdout_on_success() {
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo hello")]);
        let runtime: DummyRuntime = DummyRuntime::builder()
            .command("echo hello", succeeded("hello\n"))
            .build();
        execute_graph(&graph, &workspace_directory(), &config(Verbosity::Verbose), &runtime).unwrap();
        assert!(
            runtime.captured_output().as_str().contains("hello"),
            "verbose output should include task stdout"
        );
    }

    #[test]
    fn normal_hides_stdout_on_success() {
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo hello")]);
        let runtime: DummyRuntime = DummyRuntime::builder()
            .command("echo hello", succeeded("hello\n"))
            .build();
        execute_graph(&graph, &workspace_directory(), &default_config(), &runtime).unwrap();
        assert!(
            !runtime.captured_output().as_str().contains("hello"),
            "task stdout should be hidden at normal verbosity"
        );
    }

    // Real sleeps are spawned here to verify tasks in a step genuinely run in parallel.
    #[test]
    fn parallel_tasks_in_same_step_run_concurrently() {
        let graph: TaskGraph = make_graph(vec![
            ("sleep-a", "compile", "sleep 0.3"),
            ("sleep-b", "compile", "sleep 0.3"),
        ]);
        let start: Instant = Instant::now();
        execute_graph(&graph, &working_directory(), &default_config(), &system_runtime()).unwrap();
        let elapsed: Duration = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "two 0.3s tasks in the same step should run in parallel (took {elapsed:?})"
        );
    }

    #[test]
    fn parallel_tasks_have_distinct_fibers() {
        let graph: TaskGraph = make_graph(vec![
            ("task-a", "compile", "command-a"),
            ("task-b", "compile", "command-b"),
        ]);
        let runtime: DummyRuntime = DummyRuntime::builder()
            .command("command-a", succeeded(""))
            .command("command-b", succeeded(""))
            .build();
        let outcomes: Vec<TaskOutcome> =
            execute_graph(&graph, &workspace_directory(), &default_config(), &runtime).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert_ne!(
            outcomes[0].fiber, outcomes[1].fiber,
            "parallel tasks should have distinct fibers"
        );
    }
}
