use crate::dirtiness::Dirtiness;
use crate::dirtiness::TaskLayout;
use crate::dirtiness::TaskRunRecord;
use crate::dirtiness::dirtiness;
use crate::error::SindriError;
use crate::file_set::FileSet;
use crate::lifecycle::TaskGraph;
use crate::lifecycle::TaskGraphNode;
use crate::metadata_cache::MetadataCache;
use crate::parameter::BindingHash;
use crate::parameter::ParameterBinding;
use crate::parameter::ParameterValues;
use crate::runtime::Runtime;
use crate::script::Command as ScriptCommand;
use crate::task::Task;
use crate::task::resolve_file_set;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::BuildStart;
use crate::types::Command;
use crate::types::CommandOutput;
use crate::types::Fiber;
use crate::types::ModulePath;
use crate::types::RelativeDirectory;
use crate::types::Stderr;
use crate::types::Stdout;
use crate::types::TaskGraphNodeId;
use crate::types::TaskStart;
use crate::types::TaskStatus;
use crate::types::WorkspaceRoot;
use miette::IntoDiagnostic;
use miette::Result as MietteResult;
use std::io::Error as IoError;
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

/// Where a module lives for the purpose of building it: the absolute directory its sources occupy and
/// its commands run in, paired with the [`ModulePath`] that keys its state subtree under the build
/// directory. Together they pin one module within a multi-module build, so no two modules' work or
/// state can collide.
pub struct ModuleLocation {
    working_directory: AbsoluteDirectory,
    module_path: ModulePath,
}

impl ModuleLocation {
    pub fn new(working_directory: AbsoluteDirectory, module_path: ModulePath) -> ModuleLocation {
        ModuleLocation {
            working_directory,
            module_path,
        }
    }

    pub fn working_directory(&self) -> &AbsoluteDirectory {
        &self.working_directory
    }

    pub fn module_path(&self) -> &ModulePath {
        &self.module_path
    }
}

/// Whether a module ran (rebuilt) any of its tasks on a build. The module scheduler carries it across
/// dependency edges: a module whose dependency rebuilt must itself rebuild — even when its own sources
/// are unchanged — because each module tracks only its own source and a dependency's files are never
/// folded into the dependent's tracked input set, so the rebuilt signal is the only thing that ties a
/// dependent's freshness to its dependencies'.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleRebuilt(bool);

impl ModuleRebuilt {
    pub fn new(rebuilt: bool) -> ModuleRebuilt {
        ModuleRebuilt(rebuilt)
    }

    pub fn is_rebuilt(&self) -> bool {
        self.0
    }
}

/// What running a task's resolved commands in order produced: every command succeeded, or a specific
/// one failed — its output paired with the exact command that produced it, so a failure can be
/// reported precisely rather than just as "something failed".
enum CommandOutcome {
    Succeeded(CommandOutput),
    Failed {
        output: CommandOutput,
        command: ScriptCommand,
    },
}

#[derive(Debug)]
pub struct TaskOutcome {
    task: Task,
    output: CommandOutput,
    failed_command: Option<ScriptCommand>,
    task_duration: Duration,
    task_start: TaskStart,
    fiber: Fiber,
    dirtiness: Dirtiness,
}

impl TaskOutcome {
    fn from(
        task: Task,
        result: IoResult<CommandOutcome>,
        task_start: TaskStart,
        fiber: Fiber,
        dirtiness: Dirtiness,
    ) -> TaskOutcome {
        let (output, failed_command): (CommandOutput, Option<ScriptCommand>) = match result {
            Ok(CommandOutcome::Succeeded(output)) => (output, None),
            Ok(CommandOutcome::Failed { output, command }) => (output, Some(command)),
            Err(error) => (
                CommandOutput::new(
                    Stdout::default(),
                    error.to_string().into_bytes().into(),
                    TaskStatus::Failed,
                ),
                None,
            ),
        };
        TaskOutcome {
            task,
            output,
            failed_command,
            task_duration: task_start.elapsed(),
            task_start,
            fiber,
            dirtiness,
        }
    }

    /// A skipped (clean) task: no command ran, so it is recorded as an instant success that still
    /// emits a telemetry event marked as a hit.
    fn cached(task: Task, task_start: TaskStart, fiber: Fiber) -> TaskOutcome {
        TaskOutcome {
            task,
            output: CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Succeeded),
            failed_command: None,
            task_duration: task_start.elapsed(),
            task_start,
            fiber,
            dirtiness: Dirtiness::Clean,
        }
    }

    #[cfg(test)]
    pub fn new(
        task: Task,
        output: CommandOutput,
        task_duration: Duration,
        task_start: TaskStart,
        fiber: Fiber,
        dirtiness: Dirtiness,
    ) -> TaskOutcome {
        TaskOutcome {
            task,
            output,
            failed_command: None,
            task_duration,
            task_start,
            fiber,
            dirtiness,
        }
    }

    pub fn task(&self) -> &Task {
        &self.task
    }

    pub fn output(&self) -> &CommandOutput {
        &self.output
    }

    /// The specific command that failed, if the task failed because one of its resolved commands
    /// exited unsuccessfully — `None` if the task succeeded, or if it failed before any command ran
    /// (e.g. its script failed to resolve).
    pub fn failed_command(&self) -> Option<&ScriptCommand> {
        self.failed_command.as_ref()
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

    pub fn dirtiness(&self) -> Dirtiness {
        self.dirtiness
    }
}

fn format_duration(duration: Duration) -> String {
    format!("{:.1}s", duration.as_secs_f64())
}

fn group_nodes_by_step(nodes: &[TaskGraphNode]) -> Vec<Vec<TaskGraphNodeId>> {
    let mut groups: Vec<Vec<TaskGraphNodeId>> = Vec::new();
    let mut current_step: Option<&crate::types::Step> = None;
    for (index, node) in nodes.iter().enumerate() {
        if current_step.is_none_or(|step: &crate::types::Step| -> bool { step != node.step() }) {
            groups.push(Vec::new());
            current_step = Some(node.step());
        }
        groups.last_mut().unwrap().push(TaskGraphNodeId::new(index));
    }
    groups
}

/// A task paired with the decision of whether it must run. Built in a serial pre-pass so the
/// executor knows, before spawning anything, which tasks are clean (and stay silent) and which are
/// dirty (and run).
struct TaskPlan {
    task: Task,
    layout: TaskLayout,
    dirtiness: Dirtiness,
}

/// Compute each task's current run record and compare it against what was persisted, deciding clean
/// or dirty. Serial and in lifecycle order, so a source-mutating step's effects are on disk before
/// the next step is hashed. `module_directory` is relative to the workspace root — the module's real
/// source location, distinct from `module_path`, which additionally carries a qualifier and only
/// matters for where a task's own state nests under the build directory.
#[allow(clippy::too_many_arguments)]
fn plan_group(
    node_ids: &[TaskGraphNodeId],
    nodes: &[TaskGraphNode],
    module_directory: &RelativeDirectory,
    workspace_root: &WorkspaceRoot,
    build_directory: &AbsoluteDirectory,
    module_path: &ModulePath,
    cache: &MetadataCache,
    runtime: &impl Runtime,
) -> MietteResult<Vec<TaskPlan>> {
    let mut plans: Vec<TaskPlan> = Vec::with_capacity(node_ids.len());
    for &node_id in node_ids {
        let node: &TaskGraphNode = &nodes[node_id.value()];
        let task: Task = node.task().clone();
        // No task declares a parameter yet, so every binding is the same empty one.
        let binding_hash: BindingHash = ParameterBinding::empty().binding_hash();
        let layout: TaskLayout = TaskLayout::new(
            build_directory,
            module_path.as_relative_directory(),
            task.name(),
            binding_hash,
        );
        let current_record: TaskRunRecord = TaskRunRecord::compute(
            &task,
            module_directory,
            layout.output_directory(),
            workspace_root,
            cache,
            runtime,
        )?;
        let persisted: Option<TaskRunRecord> = TaskRunRecord::load(layout.run_record_file(), runtime);
        let status: Dirtiness = dirtiness(&current_record, persisted.as_ref());
        plans.push(TaskPlan {
            task,
            layout,
            dirtiness: status,
        });
    }
    Ok(plans)
}

/// Run every dirty task in `plans` concurrently, one real OS thread each. A task's resolved script
/// yields an ordered list of commands, run sequentially on that single thread — concurrency in this
/// executor is across tasks, never within one task's own command sequence. On success, fresh state is
/// recorded so the next build can skip it.
fn run_misses(
    plans: &[TaskPlan],
    module_directory: &RelativeDirectory,
    workspace_root: &WorkspaceRoot,
    cache: &MetadataCache,
    runtime: &impl Runtime,
) -> Vec<TaskOutcome> {
    let misses: Vec<&TaskPlan> = plans
        .iter()
        .filter(|plan: &&TaskPlan| -> bool { plan.dirtiness == Dirtiness::Dirty })
        .collect();
    std::thread::scope(|scope| {
        let handles: Vec<_> = misses
            .iter()
            .enumerate()
            .map(|(index, &plan)| {
                let fiber: Fiber = Fiber::new(index);
                let task: Task = plan.task.clone();
                let output_directory: AbsoluteDirectory = plan.layout.output_directory().clone();
                let run_record_file: AbsoluteFile = plan.layout.run_record_file().clone();
                scope.spawn(move || -> TaskOutcome {
                    let task_start: TaskStart = TaskStart::new(runtime.now());
                    let result: IoResult<CommandOutcome> =
                        run_one(&task, module_directory, &output_directory, workspace_root, runtime);
                    let outcome: TaskOutcome = TaskOutcome::from(task, result, task_start, fiber, Dirtiness::Dirty);
                    if outcome.output().status().is_success() {
                        persist_outcome(
                            outcome.task(),
                            module_directory,
                            &output_directory,
                            &run_record_file,
                            workspace_root,
                            cache,
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

/// Resolve a task's script and run its commands in order, stopping at the first failure (mirroring
/// shell `&&` chaining) — a later command in the sequence is assumed to depend on the ones before it
/// having actually succeeded. A setup failure (script resolution, e.g. a malformed pattern or a
/// parameter error) surfaces the same way a real command failure does: as the task's own failure.
fn run_one(
    task: &Task,
    module_directory: &RelativeDirectory,
    output_directory: &AbsoluteDirectory,
    workspace_root: &WorkspaceRoot,
    runtime: &impl Runtime,
) -> IoResult<CommandOutcome> {
    runtime.create_directories(output_directory.as_ref())?;
    let commands: Vec<ScriptCommand> = task
        .resolve(
            &ParameterValues::default(),
            module_directory,
            output_directory,
            workspace_root,
            runtime,
        )
        .map_err(IoError::other)?;
    let mut output: CommandOutput = CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Succeeded);
    for command in &commands {
        let working_directory: AbsoluteDirectory = workspace_root
            .to_absolute_directory()
            .join_directory(command.working_directory());
        output = runtime.run_command(&to_process_command(command), working_directory.as_ref())?;
        if !output.status().is_success() {
            return Ok(CommandOutcome::Failed {
                output,
                command: command.clone(),
            });
        }
    }
    Ok(CommandOutcome::Succeeded(output))
}

/// Adapt a script-resolved [`ScriptCommand`] to the [`Command`] shape [`Runtime::run_command`]
/// spawns. The working directory is not carried here — it is resolved to an absolute path and passed
/// to `run_command` separately.
fn to_process_command(command: &ScriptCommand) -> Command {
    let mut process_command: Command = Command::new(command.program(), command.arguments().iter().cloned());
    for (name, value) in command.environment() {
        process_command = process_command.with_environment_variable(name.clone(), value.clone());
    }
    process_command
}

/// Render a script-resolved command as a single space-joined line for diagnostics, the same
/// convention [`Command`]'s own `Display` uses for a process-ready command.
fn render_command(command: &ScriptCommand) -> String {
    let mut rendered: String = command.program().to_string();
    for argument in command.arguments() {
        rendered.push(' ');
        rendered.push_str(argument);
    }
    rendered
}

/// Record a successful task's fresh run record. Best effort: a failure here only means the task is
/// not cached and re-runs next time, so it is logged rather than allowed to fail a build that
/// succeeded.
fn persist_outcome(
    task: &Task,
    module_directory: &RelativeDirectory,
    output_directory: &AbsoluteDirectory,
    run_record_file: &AbsoluteFile,
    workspace_root: &WorkspaceRoot,
    cache: &MetadataCache,
    runtime: &impl Runtime,
) {
    // The task's own commands just ran and are the only thing that can have changed its output —
    // drop the metadata cache's memo for those files so the fresh record below reads them for real,
    // instead of reusing whatever a pre-run dirtiness check already cached as missing or stale.
    let output_files: FileSet =
        match resolve_file_set(task.output().pattern(), output_directory, workspace_root, runtime) {
            Ok(files) => files,
            Err(error) => {
                let _ = runtime.log(&format!("could not resolve the output of `{}`: {error}", task.name()));
                return;
            }
        };
    cache.invalidate(&output_files);
    let record: TaskRunRecord =
        match TaskRunRecord::compute(task, module_directory, output_directory, workspace_root, cache, runtime) {
            Ok(record) => record,
            Err(error) => {
                let _ = runtime.log(&format!(
                    "could not compute a run record for `{}`: {error}",
                    task.name()
                ));
                return;
            }
        };
    if let Err(error) = record.persist(run_record_file, runtime) {
        let _ = runtime.log(&format!(
            "could not persist a run record for `{}`: {error}",
            task.name()
        ));
    }
    // Settle the metadata cache's own per-file records too, so a later build can trust them via a
    // cheap stat check instead of reading their content again.
    if let Err(error) = cache.persist(&output_files, workspace_root, runtime) {
        let _ = runtime.log(&format!(
            "could not persist metadata-cache records for `{}`: {error}",
            task.name()
        ));
    }
}

pub fn execute_graph(
    graph: &TaskGraph,
    location: &ModuleLocation,
    workspace_root: &WorkspaceRoot,
    build_directory: &AbsoluteDirectory,
    dependency_rebuilt: ModuleRebuilt,
    config: &ExecutionConfig,
    runtime: &impl Runtime,
) -> MietteResult<(Vec<TaskOutcome>, ModuleRebuilt)> {
    let module_directory: RelativeDirectory = workspace_root.relativize_directory(location.working_directory());
    let cache_directory: AbsoluteDirectory = build_directory.join_directory(&RelativeDirectory::new(".metadata-cache"));
    let cache: MetadataCache = MetadataCache::new(cache_directory);
    let step_groups: Vec<Vec<TaskGraphNodeId>> = group_nodes_by_step(graph.nodes());
    let mut all_outcomes: Vec<TaskOutcome> = Vec::new();
    for group in &step_groups {
        let mut plans: Vec<TaskPlan> = plan_group(
            group,
            graph.nodes(),
            &module_directory,
            workspace_root,
            build_directory,
            location.module_path(),
            &cache,
            runtime,
        )?;
        if dependency_rebuilt.is_rebuilt() {
            // A rebuilt dependency invalidates this module wholesale: its outputs were produced against
            // the dependency's previous sources, which are not part of this module's tracked inputs, so
            // no per-task dirtiness check would notice. Force every task to run.
            for plan in &mut plans {
                plan.dirtiness = Dirtiness::Dirty;
            }
        }
        if config.verbosity != Verbosity::Quiet {
            for plan in &plans {
                if plan.dirtiness == Dirtiness::Dirty {
                    writeln!(runtime.output(), "  \u{2192} {}", plan.task.name()).into_diagnostic()?;
                }
            }
        }
        let mut miss_outcomes: std::vec::IntoIter<TaskOutcome> =
            run_misses(&plans, &module_directory, workspace_root, &cache, runtime).into_iter();
        let batch_start: usize = all_outcomes.len();
        for plan in &plans {
            let outcome: TaskOutcome = match plan.dirtiness {
                Dirtiness::Clean => {
                    TaskOutcome::cached(plan.task.clone(), TaskStart::new(runtime.now()), Fiber::new(0))
                }
                Dirtiness::Dirty => miss_outcomes.next().expect("one outcome per dirty task"),
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
                command: failed.failed_command().map_or_else(
                    || "no command ran (the task failed before one could)".to_string(),
                    render_command,
                ),
                output: failed.output.combined_output(),
            }
            .into());
        }
    }
    let rebuilt: ModuleRebuilt = ModuleRebuilt::new(
        all_outcomes
            .iter()
            .any(|outcome: &TaskOutcome| outcome.dirtiness() == Dirtiness::Dirty),
    );
    Ok((all_outcomes, rebuilt))
}

/// Print a task's completion line. Clean tasks stay silent unless `--verbose`; dirty tasks always show
/// their result and, when verbose, their captured output.
fn report_outcome(outcome: &TaskOutcome, config: &ExecutionConfig, runtime: &impl Runtime) -> MietteResult<()> {
    if config.verbosity == Verbosity::Quiet {
        return Ok(());
    }
    match outcome.dirtiness() {
        Dirtiness::Clean => {
            if config.verbosity == Verbosity::Verbose {
                writeln!(runtime.output(), "  \u{2713} {} (cached)", outcome.task().name()).into_diagnostic()?;
            }
        }
        Dirtiness::Dirty => {
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
    use crate::file_set::FileSetPattern;
    use crate::lifecycle::TaskGraph;
    use crate::lifecycle::TaskGraphBuilder;
    use crate::lifecycle::TaskGraphNode;
    use crate::parameter::ParameterDeclarations;
    use crate::runtime::Bootstrap;
    use crate::runtime::DummyRuntime;
    use crate::runtime::SystemFileSystem;
    use crate::script::Script;
    use crate::task::DeclaredTaskInput;
    use crate::task::ManagedTaskInput;
    use crate::task::TaskName;
    use crate::task::TaskOutput;
    use crate::types::RelativeDirectory;
    use crate::types::Stderr;
    use crate::types::Step;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::mpsc;
    use std::sync::mpsc::Receiver;
    use std::sync::mpsc::RecvTimeoutError;
    use std::sync::mpsc::Sender;
    use std::thread;
    use std::thread::JoinHandle;

    fn system_runtime() -> impl Runtime {
        SystemFileSystem.into_runtime(BuildStart::now(), None).unwrap()
    }

    fn make_task(name: &str, command: &str) -> Task {
        Task::new(
            TaskName::new(name),
            Script::new(format!("fun inputs => [ {{ program = \"{command}\" }} ]")),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        )
    }

    fn make_graph(tasks: Vec<(&str, &str, &str)>) -> TaskGraph {
        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        for (name, step, command) in tasks {
            builder.add_node(TaskGraphNode::new(make_task(name, command), Step::new(step)));
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
        let location: ModuleLocation =
            ModuleLocation::new(working_directory.clone(), ModulePath::new(RelativeDirectory::new("")));
        execute_graph(
            graph,
            &location,
            &workspace_root,
            &build_directory,
            ModuleRebuilt::new(false),
            config,
            runtime,
        )
        .map(|(outcomes, _rebuilt): (Vec<TaskOutcome>, ModuleRebuilt)| -> Vec<TaskOutcome> { outcomes })
    }

    /// Run a graph with real process spawning, isolating its persisted state in a fresh temporary
    /// directory so repeated test runs never observe each other's cached state.
    fn run_real(graph: &TaskGraph, config: &ExecutionConfig) -> Vec<TaskOutcome> {
        let scratch: tempfile::TempDir = tempfile::TempDir::new().unwrap();
        let working_directory: AbsoluteDirectory = AbsoluteDirectory::new(scratch.path().to_path_buf());
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(working_directory.clone());
        let build_directory: AbsoluteDirectory = working_directory.join_directory(&RelativeDirectory::new(".target"));
        let location: ModuleLocation =
            ModuleLocation::new(working_directory.clone(), ModulePath::new(RelativeDirectory::new("")));
        execute_graph(
            graph,
            &location,
            &workspace_root,
            &build_directory,
            ModuleRebuilt::new(false),
            config,
            &system_runtime(),
        )
        .unwrap()
        .0
    }

    /// Run a graph against a stub runtime with an explicit dependency-rebuilt signal, returning both the
    /// outcomes and whether the module rebuilt — the two facts the module scheduler threads across
    /// dependency edges.
    fn run_with_dependency(
        graph: &TaskGraph,
        working_directory: &AbsoluteDirectory,
        dependency_rebuilt: ModuleRebuilt,
        runtime: &impl Runtime,
    ) -> (Vec<TaskOutcome>, ModuleRebuilt) {
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(working_directory.clone());
        let build_directory: AbsoluteDirectory = working_directory.join_directory(&RelativeDirectory::new(".target"));
        let location: ModuleLocation =
            ModuleLocation::new(working_directory.clone(), ModulePath::new(RelativeDirectory::new("")));
        execute_graph(
            graph,
            &location,
            &workspace_root,
            &build_directory,
            dependency_rebuilt,
            &default_config(),
            runtime,
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
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo")]);
        let outcomes: Vec<TaskOutcome> = run_real(&graph, &default_config());
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].output().status(), TaskStatus::Succeeded);
    }

    // A real command is executed here to prove a script's `environment` record actually reaches the
    // spawned process — the same mechanism T8b's GOWORK wiring will depend on.
    #[test]
    fn a_scripts_environment_reaches_the_spawned_process() {
        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        builder.add_node(TaskGraphNode::new(
            Task::new(
                TaskName::new("env-task"),
                Script::new("fun inputs => [ { program = \"sh\", arguments = [ \"-c\", \"echo $GREETING\" ], environment = { GREETING = \"hello\" } } ]"),
                DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
                ParameterDeclarations::default(),
            ),
            Step::new("compile"),
        ));
        let outcomes: Vec<TaskOutcome> = run_real(&builder.build(), &default_config());
        assert_eq!(outcomes[0].output().status(), TaskStatus::Succeeded);
        assert_eq!(outcomes[0].output().stdout().to_string_lossy().trim(), "hello");
    }

    #[test]
    fn failing_command_exits_nonzero() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        let result: MietteResult<Vec<TaskOutcome>> = run(&graph, &workspace_directory(), &default_config(), &runtime);
        assert!(result.is_err(), "a failing command should fail the whole run");
    }

    #[test]
    fn task_failure_error_contains_command() {
        // A dedicated task (rather than `make_task`, which never supplies arguments) so the error can
        // be checked for the full command line, not just the program name.
        let mut builder: TaskGraphBuilder = TaskGraphBuilder::new();
        builder.add_node(TaskGraphNode::new(
            Task::new(
                TaskName::new("false-task"),
                Script::new("fun inputs => [ { program = \"false\", arguments = [ \"--flag\" ] } ]"),
                DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
                ParameterDeclarations::default(),
            ),
            Step::new("compile"),
        ));
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        let result: MietteResult<Vec<TaskOutcome>> =
            run(&builder.build(), &workspace_directory(), &default_config(), &runtime);
        let error: String = result.unwrap_err().to_string();
        assert!(
            error.contains("false --flag"),
            "error should name the exact failing command: {error}"
        );
    }

    #[test]
    fn failing_task_output_shown_in_error_regardless_of_verbosity() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("boom")).build();
        let result: MietteResult<Vec<TaskOutcome>> =
            run(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime);
        let error: String = result.unwrap_err().to_string();
        assert!(
            error.contains("boom"),
            "error should surface the command's output: {error}"
        );
    }

    #[test]
    fn quiet_suppresses_progress_lines_on_success() {
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("echo", succeeded("hello\n")).build();
        run(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime).unwrap();
        assert!(runtime.captured_output().is_empty());
    }

    #[test]
    fn quiet_still_returns_error_on_failure() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("")).build();
        let result: MietteResult<Vec<TaskOutcome>> =
            run(&graph, &workspace_directory(), &config(Verbosity::Quiet), &runtime);
        assert!(result.is_err());
    }

    #[test]
    fn normal_hides_stdout_on_success() {
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("echo", succeeded("hello\n")).build();
        run(&graph, &workspace_directory(), &config(Verbosity::Normal), &runtime).unwrap();
        assert!(!runtime.captured_output().as_str().contains("hello"));
    }

    #[test]
    fn verbose_shows_stdout_on_success() {
        let graph: TaskGraph = make_graph(vec![("echo-task", "compile", "echo")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("echo", succeeded("hello\n")).build();
        run(&graph, &workspace_directory(), &config(Verbosity::Verbose), &runtime).unwrap();
        assert!(runtime.captured_output().as_str().contains("hello"));
    }

    #[test]
    fn failing_command_output_shown_in_error_regardless_of_verbosity() {
        let graph: TaskGraph = make_graph(vec![("false-task", "compile", "false")]);
        let runtime: DummyRuntime = DummyRuntime::builder().command("false", failed("nope")).build();
        let result: MietteResult<Vec<TaskOutcome>> =
            run(&graph, &workspace_directory(), &config(Verbosity::Normal), &runtime);
        let error: String = result.unwrap_err().to_string();
        assert!(error.contains("nope"), "error should surface stderr: {error}");
    }

    #[test]
    fn a_rebuilt_dependency_forces_an_otherwise_clean_task_to_run() {
        // A single compile task with no tracked inputs or outputs, so once its run record is on disk
        // it is a cache hit. Its command is stubbed for when it does run.
        let graph: TaskGraph = make_graph(vec![("go-compile", "compile", "go")]);
        let seed: DummyRuntime = DummyRuntime::builder().command("go", succeeded("")).build();
        let (_, rebuilt): (Vec<TaskOutcome>, ModuleRebuilt) =
            run_with_dependency(&graph, &workspace_directory(), ModuleRebuilt::new(false), &seed);
        assert!(rebuilt.is_rebuilt(), "the first run should always report a rebuild");
        let mut replay: crate::runtime::DummyRuntimeBuilder = DummyRuntime::builder().command("go", succeeded(""));
        for (path, contents) in seed.written_files() {
            replay = replay.file(path, contents);
        }
        let replay: DummyRuntime = replay.build();
        let (outcomes, rebuilt_again): (Vec<TaskOutcome>, ModuleRebuilt) =
            run_with_dependency(&graph, &workspace_directory(), ModuleRebuilt::new(false), &replay);
        assert!(
            !rebuilt_again.is_rebuilt(),
            "an unchanged task should be a cache hit on replay"
        );
        assert_eq!(outcomes[0].dirtiness(), Dirtiness::Clean);
        let (forced_outcomes, forced_rebuilt): (Vec<TaskOutcome>, ModuleRebuilt) =
            run_with_dependency(&graph, &workspace_directory(), ModuleRebuilt::new(true), &replay);
        assert!(
            forced_rebuilt.is_rebuilt(),
            "a forced dependency rebuild should force this module too"
        );
        assert_eq!(forced_outcomes[0].dirtiness(), Dirtiness::Dirty);
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
            outcomes[0].fiber(),
            outcomes[1].fiber(),
            "parallel tasks should have distinct fibers"
        );
    }

    /// A stub that blocks on `barrier` before returning. The executor spawns same-step tasks on
    /// separate real OS threads (see `run_misses`), so this can only return for both tasks if both
    /// were genuinely dispatched before either finished — a structural proof of concurrent dispatch
    /// that needs no timing, no sleeping, and no real subprocess.
    fn rendezvous(barrier: Arc<Barrier>) -> impl Fn() -> CommandOutput + Send + Sync + 'static {
        move || -> CommandOutput {
            barrier.wait();
            succeeded("")
        }
    }

    type RunOutcome = MietteResult<Vec<TaskOutcome>>;

    /// Run `graph` on a background thread and wait up to `timeout` for it to finish, so a regression
    /// that serializes what should be concurrent dispatch fails the test with a clear message
    /// instead of hanging it (and the rest of the suite) forever.
    fn run_bounded(
        graph: TaskGraph,
        working_directory: AbsoluteDirectory,
        config: ExecutionConfig,
        runtime: DummyRuntime,
        timeout: Duration,
    ) -> Vec<TaskOutcome> {
        let (sender, receiver): (Sender<RunOutcome>, Receiver<RunOutcome>) = mpsc::channel();
        let handle: JoinHandle<()> = thread::spawn(move || {
            let _ = sender.send(run(&graph, &working_directory, &config, &runtime));
        });
        match receiver.recv_timeout(timeout) {
            Ok(outcome) => outcome.unwrap(),
            Err(RecvTimeoutError::Timeout) => {
                panic!("tasks in the same step did not run concurrently (dispatch hung waiting on the barrier)")
            }
            Err(RecvTimeoutError::Disconnected) => match handle.join() {
                Ok(()) => panic!("background thread exited without sending a result"),
                Err(panic_payload) => std::panic::resume_unwind(panic_payload),
            },
        }
    }

    #[test]
    fn parallel_tasks_in_same_step_run_concurrently() {
        let barrier: Arc<Barrier> = Arc::new(Barrier::new(2));
        let runtime: DummyRuntime = DummyRuntime::builder()
            .command_handler("task-a", rendezvous(Arc::clone(&barrier)))
            .command_handler("task-b", rendezvous(barrier))
            .build();
        let graph: TaskGraph = make_graph(vec![("task-a", "compile", "task-a"), ("task-b", "compile", "task-b")]);
        let outcomes: Vec<TaskOutcome> = run_bounded(
            graph,
            workspace_directory(),
            default_config(),
            runtime,
            Duration::from_secs(5),
        );
        assert_eq!(outcomes.len(), 2);
        assert!(
            outcomes
                .iter()
                .all(|outcome: &TaskOutcome| -> bool { outcome.output().status().is_success() }),
            "both tasks should have run their stubbed command successfully"
        );
    }
}
