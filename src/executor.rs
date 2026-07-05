use crate::error::SindriError;
use crate::hash::DeclarationHash;
use crate::hash::FileSetHash;
use crate::lifecycle::TaskGraph;
use crate::lifecycle::TaskGraphNode;
use crate::plugin::Task;
use crate::runtime::Runtime;
use crate::state::CacheStatus;
use crate::state::TaskPaths;
use crate::state::TaskState;
use crate::state::dirtiness;
use crate::state::file_set_hash;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::BuildStart;
use crate::types::Command;
use crate::types::CommandOutput;
use crate::types::Fiber;
use crate::types::ModulePath;
use crate::types::Stderr;
use crate::types::Stdout;
use crate::types::Step;
use crate::types::TaskGraphNodeId;
use crate::types::TaskStart;
use crate::types::TaskStatus;
use crate::types::WorkspaceRoot;
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
    cache: CacheStatus,
}

impl TaskOutcome {
    pub fn from(
        task: Task,
        result: IoResult<CommandOutput>,
        task_start: TaskStart,
        fiber: Fiber,
        cache: CacheStatus,
    ) -> TaskOutcome {
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
            cache,
        }
    }

    /// A skipped (cache-hit) task: no command ran, so it is recorded as an instant success that still
    /// emits a telemetry event marked as a hit.
    fn cached(task: Task, task_start: TaskStart, fiber: Fiber) -> TaskOutcome {
        TaskOutcome {
            task,
            output: CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Succeeded),
            task_duration: task_start.elapsed(),
            task_start,
            fiber,
            cache: CacheStatus::Hit,
        }
    }

    #[cfg(test)]
    pub fn new(
        task: Task,
        output: CommandOutput,
        task_duration: Duration,
        task_start: TaskStart,
        fiber: Fiber,
        cache: CacheStatus,
    ) -> TaskOutcome {
        TaskOutcome {
            task,
            output,
            task_duration,
            task_start,
            fiber,
            cache,
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

    pub fn cache(&self) -> CacheStatus {
        self.cache
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

/// A task paired with the decision of whether it must run. Built in a serial pre-pass so the
/// executor knows, before spawning anything, which tasks are cache hits (and stay silent) and which
/// are misses (and run).
struct TaskPlan {
    task: Task,
    paths: TaskPaths,
    current_state: TaskState,
    cache: CacheStatus,
}

/// Compute each task's current state and compare it against what was persisted, deciding hit or
/// miss. Serial and in lifecycle order, so a source-mutating step's effects are on disk before the
/// next step is hashed.
fn plan_group(
    node_ids: &[TaskGraphNodeId],
    nodes: &[TaskGraphNode],
    working_directory: &AbsoluteDirectory,
    workspace_root: &WorkspaceRoot,
    build_directory: &AbsoluteDirectory,
    module_path: &ModulePath,
    runtime: &impl Runtime,
) -> MietteResult<Vec<TaskPlan>> {
    let mut plans: Vec<TaskPlan> = Vec::with_capacity(node_ids.len());
    for &node_id in node_ids {
        let node: &TaskGraphNode = &nodes[node_id.value()];
        let task: Task = node.task().clone();
        let paths: TaskPaths = TaskPaths::new(build_directory, module_path, node.step(), task.name());
        let current_state: TaskState = TaskState::compute(
            &task,
            working_directory,
            paths.output_directory(),
            workspace_root,
            runtime,
        )
        .map_err(|source| SindriError::Io {
            path: working_directory.as_ref().to_path_buf(),
            source,
        })?;
        let persisted: Option<TaskState> = TaskState::load(paths.state_file(), runtime);
        let cache: CacheStatus = dirtiness(&current_state, persisted.as_ref());
        plans.push(TaskPlan {
            task,
            paths,
            current_state,
            cache,
        });
    }
    Ok(plans)
}

/// Run every miss in `plans` concurrently. Each task spawns its command into its own `output/`
/// directory (with `{output}` resolved) and, on success, records fresh state so the next build can
/// skip it.
fn run_misses(
    plans: &[TaskPlan],
    working_directory: &AbsoluteDirectory,
    workspace_root: &WorkspaceRoot,
    runtime: &impl Runtime,
) -> Vec<TaskOutcome> {
    let misses: Vec<&TaskPlan> = plans
        .iter()
        .filter(|plan: &&TaskPlan| -> bool { plan.cache == CacheStatus::Miss })
        .collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = misses
            .iter()
            .enumerate()
            .map(|(index, &plan)| {
                let fiber: Fiber = Fiber::new(index);
                let task: Task = plan.task.clone();
                let output_directory: AbsoluteDirectory = plan.paths.output_directory().clone();
                let state_file: AbsoluteFile = plan.paths.state_file().clone();
                let inputs: FileSetHash = plan.current_state.inputs();
                let declaration: DeclarationHash = plan.current_state.declaration();
                scope.spawn(move || -> TaskOutcome {
                    let task_start: TaskStart = TaskStart::new(runtime.now());
                    let result: IoResult<CommandOutput> = run_one(&task, &output_directory, working_directory, runtime);
                    let outcome: TaskOutcome = TaskOutcome::from(task, result, task_start, fiber, CacheStatus::Miss);
                    if outcome.output().status().is_success() {
                        persist_outcome(
                            outcome.task(),
                            &output_directory,
                            &state_file,
                            inputs,
                            declaration,
                            workspace_root,
                            runtime,
                        );
                    }
                    outcome
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle: ScopedJoinHandle<'_, TaskOutcome>| -> TaskOutcome { handle.join().unwrap() })
            .collect()
    })
}

/// Create a task's output directory, resolve `{output}`, and spawn the command. A setup failure
/// surfaces as the task's command failure.
fn run_one(
    task: &Task,
    output_directory: &AbsoluteDirectory,
    working_directory: &AbsoluteDirectory,
    runtime: &impl Runtime,
) -> IoResult<CommandOutput> {
    runtime.create_directories(output_directory.as_ref())?;
    let command: Command = task.command().with_output_directory(output_directory.as_ref());
    runtime.run_command(&command, working_directory.as_ref())
}

/// Record a successful task's fresh state. Best effort: a failure here only means the task is not
/// cached and re-runs next time, so it is logged rather than allowed to fail a build that succeeded.
fn persist_outcome(
    task: &Task,
    output_directory: &AbsoluteDirectory,
    state_file: &AbsoluteFile,
    inputs: FileSetHash,
    declaration: DeclarationHash,
    workspace_root: &WorkspaceRoot,
    runtime: &impl Runtime,
) {
    let outputs: FileSetHash = match file_set_hash(output_directory, task.outputs(), workspace_root, runtime) {
        Ok(outputs) => outputs,
        Err(error) => {
            let _ = runtime.log(&format!("could not hash outputs for `{}`: {error}", task.name()));
            return;
        }
    };
    let state: TaskState = TaskState::new(inputs, outputs, declaration);
    if let Err(error) = state.persist(state_file, runtime) {
        let _ = runtime.log(&format!("could not persist state for `{}`: {error}", task.name()));
    }
}

pub fn execute_graph(
    graph: &TaskGraph,
    working_directory: &AbsoluteDirectory,
    workspace_root: &WorkspaceRoot,
    build_directory: &AbsoluteDirectory,
    module_path: &ModulePath,
    config: &ExecutionConfig,
    runtime: &impl Runtime,
) -> MietteResult<Vec<TaskOutcome>> {
    let step_groups: Vec<Vec<TaskGraphNodeId>> = group_nodes_by_step(graph.nodes());
    let mut all_outcomes: Vec<TaskOutcome> = Vec::new();
    for group in &step_groups {
        let plans: Vec<TaskPlan> = plan_group(
            group,
            graph.nodes(),
            working_directory,
            workspace_root,
            build_directory,
            module_path,
            runtime,
        )?;
        if config.verbosity != Verbosity::Quiet {
            for plan in &plans {
                if plan.cache == CacheStatus::Miss {
                    writeln!(runtime.output(), "  \u{2192} {}", plan.task.name()).into_diagnostic()?;
                }
            }
        }
        let mut miss_outcomes: std::vec::IntoIter<TaskOutcome> =
            run_misses(&plans, working_directory, workspace_root, runtime).into_iter();
        let batch_start: usize = all_outcomes.len();
        for plan in &plans {
            let outcome: TaskOutcome = match plan.cache {
                CacheStatus::Hit => {
                    TaskOutcome::cached(plan.task.clone(), TaskStart::new(runtime.now()), Fiber::new(0))
                }
                CacheStatus::Miss => miss_outcomes.next().expect("one outcome per miss task"),
            };
            all_outcomes.push(outcome);
        }
        let batch: &[TaskOutcome] = &all_outcomes[batch_start..];
        let mut failure_index: Option<usize> = None;
        for (local_index, outcome) in batch.iter().enumerate() {
            report_outcome(outcome, config, runtime)?;
            if !outcome.output().status().is_success() && failure_index.is_none() {
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

/// Print a task's completion line. Cache hits stay silent unless `--verbose`; misses always show
/// their result and, when verbose, their captured output.
fn report_outcome(outcome: &TaskOutcome, config: &ExecutionConfig, runtime: &impl Runtime) -> MietteResult<()> {
    if config.verbosity == Verbosity::Quiet {
        return Ok(());
    }
    match outcome.cache() {
        CacheStatus::Hit => {
            if config.verbosity == Verbosity::Verbose {
                writeln!(runtime.output(), "  \u{2713} {} (cached)", outcome.task().name()).into_diagnostic()?;
            }
        }
        CacheStatus::Miss => {
            let symbol: &str = if outcome.output().status().is_success() {
                "\u{2713}"
            } else {
                "\u{2717}"
            };
            writeln!(
                runtime.output(),
                "  {} {} ({})",
                symbol,
                outcome.task().name(),
                format_duration(outcome.task_duration())
            )
            .into_diagnostic()?;
            if outcome.output().status().is_success() && config.verbosity == Verbosity::Verbose {
                let combined: String = outcome.output().combined_output();
                if !combined.is_empty() {
                    writeln!(runtime.output(), "{combined}").into_diagnostic()?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glob::GlobPatterns;
    use crate::lifecycle::TaskGraph;
    use crate::lifecycle::TaskGraphBuilder;
    use crate::lifecycle::TaskGraphNode;
    use crate::plugin::Task;
    use crate::plugin::TaskName;
    use crate::runtime::Bootstrap;
    use crate::runtime::DummyRuntime;
    use crate::runtime::SystemFileSystem;
    use crate::types::Command;
    use crate::types::RelativeDirectory;
    use crate::types::Stderr;
    use crate::types::Step;
    use std::path::PathBuf;
    use std::time::Instant;
    use tempfile::TempDir;

    fn system_runtime() -> impl Runtime {
        SystemFileSystem.into_runtime(BuildStart::now(), None).unwrap()
    }

    /// Splits a whitespace-separated command line into a [`Command`]. A test-only convenience —
    /// real callers construct the program and arguments explicitly.
    fn parse_command(command: &str) -> Command {
        let mut parts: std::str::SplitWhitespace<'_> = command.split_whitespace();
        let program: &str = parts.next().unwrap_or("");
        Command::new(program, parts)
    }

    fn make_graph(tasks: Vec<(&str, &str, &str)>) -> TaskGraph {
        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        for (name, step, command) in tasks {
            builder.add_node(TaskGraphNode::new(
                Task::new(
                    TaskName::new(name),
                    Step::new(step),
                    parse_command(command),
                    GlobPatterns::new(vec![], vec![]),
                    GlobPatterns::new(vec![], vec![]),
                ),
                Step::new(step),
            ));
        }
        builder.build()
    }

    fn workspace_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace"))
    }

    /// Run a graph against a stub runtime, deriving the incremental state directories from the given
    /// working directory. The stub records writes rather than touching disk, so the paths are fake.
    fn run(
        graph: &TaskGraph,
        working_directory: &AbsoluteDirectory,
        config: &ExecutionConfig,
        runtime: &impl Runtime,
    ) -> MietteResult<Vec<TaskOutcome>> {
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(working_directory.clone());
        let build_directory: AbsoluteDirectory = working_directory.join_directory(&RelativeDirectory::new(".target"));
        execute_graph(
            graph,
            working_directory,
            &workspace_root,
            &build_directory,
            &ModulePath::new(RelativeDirectory::new("")),
            config,
            runtime,
        )
    }

    /// Run a graph with real process spawning, isolating its persisted state in a fresh temporary
    /// directory so repeated test runs never observe each other's cached state.
    fn run_real(graph: &TaskGraph, config: &ExecutionConfig) -> Vec<TaskOutcome> {
        let scratch: TempDir = TempDir::new().unwrap();
        let working_directory: AbsoluteDirectory = AbsoluteDirectory::new(scratch.path().to_path_buf());
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(working_directory.clone());
        let build_directory: AbsoluteDirectory = working_directory.join_directory(&RelativeDirectory::new(".target"));
        execute_graph(
            graph,
            &working_directory,
            &workspace_root,
            &build_directory,
            &ModulePath::new(RelativeDirectory::new("")),
            config,
            &system_runtime(),
        )
        .unwrap()
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
        let outcomes: Vec<TaskOutcome> = run_real(&graph, &default_config());
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].output().status(), TaskStatus::Succeeded);
        assert_eq!(outcomes[0].output().stdout().as_bytes().trim_ascii_end(), b"hello");
    }

    // A real command is executed here to prove the structured command passes an argument
    // containing whitespace as a single argument rather than splitting it.
    #[test]
    fn argument_with_a_space_is_passed_as_one_argument() {
        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        builder.add_node(TaskGraphNode::new(
            Task::new(
                TaskName::new("echo-task"),
                Step::new("compile"),
                Command::new("echo", ["hello world"]),
                GlobPatterns::new(vec![], vec![]),
                GlobPatterns::new(vec![], vec![]),
            ),
            Step::new("compile"),
        ));
        let graph: TaskGraph = builder.build();
        let outcomes: Vec<TaskOutcome> = run_real(&graph, &default_config());
        assert_eq!(
            outcomes[0].output().stdout().as_bytes().trim_ascii_end(),
            b"hello world"
        );
    }

    #[test]
    fn failing_command_exits_nonzero() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        let result: MietteResult<Vec<TaskOutcome>> = run(&graph, &workspace_directory(), &default_config(), &runtime);
        assert!(result.is_err());
    }

    #[test]
    fn task_failure_error_contains_command() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        let error: miette::Error = run(&graph, &workspace_directory(), &default_config(), &runtime).unwrap_err();
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
        run(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime).unwrap();
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
            run(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime).is_err(),
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
            run(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime).unwrap_err();
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
        run(&graph, &workspace_directory(), &config(Verbosity::Verbose), &runtime).unwrap();
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
        run(&graph, &workspace_directory(), &default_config(), &runtime).unwrap();
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
        run_real(&graph, &default_config());
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
        let outcomes: Vec<TaskOutcome> = run(&graph, &workspace_directory(), &default_config(), &runtime).unwrap();
        assert_eq!(outcomes.len(), 2);
        assert_ne!(
            outcomes[0].fiber, outcomes[1].fiber,
            "parallel tasks should have distinct fibers"
        );
    }
}
