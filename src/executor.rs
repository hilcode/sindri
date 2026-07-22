use crate::dirtiness::Dirtiness;
use crate::dirtiness::TaskLayout;
use crate::dirtiness::TaskRunRecord;
use crate::dirtiness::dirtiness;
use crate::error::SindriError;
use crate::file_set::FileSet;
use crate::lifecycle::TaskGraph;
use crate::lifecycle::TaskGraphNode;
use crate::metadata_cache::MetadataCache;
use crate::parameter::ParameterBinding;
use crate::parameter::ParameterState;
use crate::runtime::Runtime;
use crate::script::Command;
use crate::task::Task;
use crate::task::resolve_file_set;
use crate::types::AbsoluteDirectory;
use crate::types::BuildStart;
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

/// The values a task resolves against that stay constant across a whole build, bundled so they thread
/// through the resolve/dirtiness/run/persist call chain as one parameter instead of four:
/// `workspace_root` and `build_directory` never change once a build starts, `managed_input_base` —
/// `generate-go-work`'s own output directory — is fixed the moment that task has run, before any
/// module's tasks resolve against it, and `cache` is the one metadata cache every task's dirtiness
/// check and run record shares for the whole build.
pub struct BuildContext<'context> {
    workspace_root: &'context WorkspaceRoot,
    build_directory: &'context AbsoluteDirectory,
    managed_input_base: &'context AbsoluteDirectory,
    cache: &'context MetadataCache,
}

impl<'context> BuildContext<'context> {
    pub fn new(
        workspace_root: &'context WorkspaceRoot,
        build_directory: &'context AbsoluteDirectory,
        managed_input_base: &'context AbsoluteDirectory,
        cache: &'context MetadataCache,
    ) -> BuildContext<'context> {
        BuildContext {
            workspace_root,
            build_directory,
            managed_input_base,
            cache,
        }
    }

    fn workspace_root(&self) -> &WorkspaceRoot {
        self.workspace_root
    }

    fn build_directory(&self) -> &AbsoluteDirectory {
        self.build_directory
    }

    fn managed_input_base(&self) -> &AbsoluteDirectory {
        self.managed_input_base
    }

    fn cache(&self) -> &MetadataCache {
        self.cache
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
    Failed { output: CommandOutput, command: Command },
}

#[derive(Debug)]
pub struct TaskOutcome {
    task: Task,
    output: CommandOutput,
    failed_command: Option<Command>,
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
        let (output, failed_command): (CommandOutput, Option<Command>) = match result {
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
    pub fn failed_command(&self) -> Option<&Command> {
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

/// A task's module-scoped resolution context: the module directory its declared input and script
/// resolve against, and the parameter values available to bind against its declared parameters.
/// Distinct from [`BuildContext`], which stays constant for the whole build — this varies per module,
/// and bundling the two keeps the resolve/run call chain below from growing an unbundled parameter per
/// module-scoped value.
struct ModuleContext<'context> {
    module_directory: &'context RelativeDirectory,
    parameter_state: &'context ParameterState,
}

impl<'context> ModuleContext<'context> {
    fn new(
        module_directory: &'context RelativeDirectory,
        parameter_state: &'context ParameterState,
    ) -> ModuleContext<'context> {
        ModuleContext {
            module_directory,
            parameter_state,
        }
    }
}

/// A task paired with the decision of whether it must run. Built in a serial pre-pass so the
/// executor knows, before spawning anything, which tasks are clean (and stay silent) and which are
/// dirty (and run).
struct TaskPlan {
    task: Task,
    layout: TaskLayout,
    dirtiness: Dirtiness,
}

/// Build a task's [`TaskLayout`], compute its current run record, and compare it against what was
/// persisted to decide clean or dirty — the "resolve, then check dirtiness" half of a task's
/// lifecycle. `module_path` names where the task's own state nests under the build directory,
/// distinct from `module.module_directory`, the module's real source location. The building block
/// both [`plan_group`] (one node of a module's step group) and [`run_standalone_task`] (a task scoped
/// to no module at all) resolve a task with.
fn resolve_task_plan(
    task: &Task,
    module: &ModuleContext,
    module_path: &RelativeDirectory,
    context: &BuildContext,
    runtime: &impl Runtime,
) -> MietteResult<TaskPlan> {
    let binding: ParameterBinding = ParameterBinding::resolve(task.declared_parameters(), module.parameter_state)?;
    let layout: TaskLayout = TaskLayout::new(
        context.build_directory(),
        module_path,
        task.name(),
        binding.binding_hash(),
    );
    let current_record: TaskRunRecord = TaskRunRecord::compute(
        task,
        module.module_directory,
        context.managed_input_base(),
        layout.output_directory(),
        context.workspace_root(),
        context.cache(),
        runtime,
    )?;
    let persisted: Option<TaskRunRecord> = TaskRunRecord::load(layout.run_record_file(), runtime);
    let status: Dirtiness = dirtiness(&current_record, persisted.as_ref());
    Ok(TaskPlan {
        task: task.clone(),
        layout,
        dirtiness: status,
    })
}

/// Compute every node's [`TaskPlan`], deciding clean or dirty for each. Serial and in lifecycle
/// order, so a source-mutating step's effects are on disk before the next step is hashed.
fn plan_group(
    node_ids: &[TaskGraphNodeId],
    nodes: &[TaskGraphNode],
    module: &ModuleContext,
    module_path: &ModulePath,
    context: &BuildContext,
    runtime: &impl Runtime,
) -> MietteResult<Vec<TaskPlan>> {
    node_ids
        .iter()
        .map(|&node_id: &TaskGraphNodeId| -> MietteResult<TaskPlan> {
            resolve_task_plan(
                nodes[node_id.value()].task(),
                module,
                module_path.as_relative_directory(),
                context,
                runtime,
            )
        })
        .collect()
}

/// Run a dirty task's resolved commands and, on success, persist its fresh run record — the "run,
/// then persist" half of a task's lifecycle. The building block both [`run_misses`] (spawned per
/// dirty task, one real OS thread each) and [`run_standalone_task`] (run synchronously, on the
/// calling thread) run a task with.
fn run_task_and_persist(
    task: Task,
    module: &ModuleContext,
    context: &BuildContext,
    layout: &TaskLayout,
    task_start: TaskStart,
    fiber: Fiber,
    runtime: &impl Runtime,
) -> TaskOutcome {
    let result: IoResult<CommandOutcome> = run_one(&task, module, context, layout.output_directory(), runtime);
    let outcome: TaskOutcome = TaskOutcome::from(task, result, task_start, fiber, Dirtiness::Dirty);
    if outcome.output().status().is_success() {
        persist_outcome(outcome.task(), module.module_directory, context, layout, runtime);
    }
    outcome
}

/// Run every dirty task in `plans` concurrently, one real OS thread each. A task's resolved script
/// yields an ordered list of commands, run sequentially on that single thread — concurrency in this
/// executor is across tasks, never within one task's own command sequence.
fn run_misses(
    plans: &[TaskPlan],
    module: &ModuleContext,
    context: &BuildContext,
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
                scope.spawn(move || -> TaskOutcome {
                    let task_start: TaskStart = TaskStart::new(runtime.now());
                    run_task_and_persist(task, module, context, &plan.layout, task_start, fiber, runtime)
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
    module: &ModuleContext,
    context: &BuildContext,
    output_directory: &AbsoluteDirectory,
    runtime: &impl Runtime,
) -> IoResult<CommandOutcome> {
    runtime.create_directories(output_directory.as_ref())?;
    let commands: Vec<Command> = task
        .resolve(
            module.parameter_state,
            module.module_directory,
            context.managed_input_base(),
            output_directory,
            context.workspace_root(),
            runtime,
        )
        .map_err(IoError::other)?;
    let mut output: CommandOutput = CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Succeeded);
    for command in &commands {
        output = runtime.run_command(command, context.workspace_root())?;
        if !output.status().is_success() {
            return Ok(CommandOutcome::Failed {
                output,
                command: command.clone(),
            });
        }
    }
    Ok(CommandOutcome::Succeeded(output))
}

/// Record a successful task's fresh run record. Best effort: a failure here only means the task is
/// not cached and re-runs next time, so it is logged rather than allowed to fail a build that
/// succeeded.
fn persist_outcome(
    task: &Task,
    module_directory: &RelativeDirectory,
    context: &BuildContext,
    layout: &TaskLayout,
    runtime: &impl Runtime,
) {
    // The task's own commands just ran and are the only thing that can have changed its output —
    // drop the metadata cache's memo for those files so the fresh record below reads them for real,
    // instead of reusing whatever a pre-run dirtiness check already cached as missing or stale.
    let output_files: FileSet = match resolve_file_set(
        task.output().pattern(),
        layout.output_directory(),
        context.workspace_root(),
        runtime,
    ) {
        Ok(files) => files,
        Err(error) => {
            let _ = runtime.log(&format!("could not resolve the output of `{}`: {error}", task.name()));
            return;
        }
    };
    context.cache().invalidate(&output_files);
    let record: TaskRunRecord = match TaskRunRecord::compute(
        task,
        module_directory,
        context.managed_input_base(),
        layout.output_directory(),
        context.workspace_root(),
        context.cache(),
        runtime,
    ) {
        Ok(record) => record,
        Err(error) => {
            let _ = runtime.log(&format!(
                "could not compute a run record for `{}`: {error}",
                task.name()
            ));
            return;
        }
    };
    if let Err(error) = record.persist(layout.run_record_file(), runtime) {
        let _ = runtime.log(&format!(
            "could not persist a run record for `{}`: {error}",
            task.name()
        ));
    }
    // Settle the metadata cache's own per-file records too, so a later build can trust them via a
    // cheap stat check instead of reading their content again.
    if let Err(error) = context
        .cache()
        .persist(&output_files, context.workspace_root(), runtime)
    {
        let _ = runtime.log(&format!(
            "could not persist metadata-cache records for `{}`: {error}",
            task.name()
        ));
    }
}

pub fn execute_graph(
    graph: &TaskGraph,
    location: &ModuleLocation,
    parameter_state: &ParameterState,
    context: &BuildContext,
    dependency_rebuilt: ModuleRebuilt,
    config: &ExecutionConfig,
    runtime: &impl Runtime,
) -> MietteResult<(Vec<TaskOutcome>, ModuleRebuilt)> {
    let module_directory: RelativeDirectory = context
        .workspace_root()
        .relativize_directory(location.working_directory());
    let module: ModuleContext = ModuleContext::new(&module_directory, parameter_state);
    let step_groups: Vec<Vec<TaskGraphNodeId>> = group_nodes_by_step(graph.nodes());
    let mut all_outcomes: Vec<TaskOutcome> = Vec::new();
    for group in &step_groups {
        let mut plans: Vec<TaskPlan> =
            plan_group(group, graph.nodes(), &module, location.module_path(), context, runtime)?;
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
            run_misses(&plans, &module, context, runtime).into_iter();
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
                    Command::to_string,
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

/// Resolve, check dirtiness, and — if dirty — run and persist a single task that is not scoped to any
/// one module, printing its progress line and failing the build the same way a per-module task would.
/// Currently used for exactly one task: `GoPlugin`'s `generate-go-work`, run once before the
/// per-module loop so its output (`go.work`) exists before any module's `go-compile`/`go-test` needs
/// it as a managed input. `module_directory` doubles as both the location the task's declared input
/// resolves against and the location its own state nests under the build directory — for
/// `generate-go-work` these coincide at the workspace root, so there is no need for the module-loop's
/// separate `module_path` concept here. It has no managed input of its own, so `context`'s
/// `managed_input_base` is never consulted — the caller passes a harmless placeholder (its real value
/// isn't known until this task has run: it's this task's own output directory). Returns the task's
/// outcome alongside its output directory, so the caller can thread the latter through as the managed
/// input base for every module's own tasks.
pub fn run_standalone_task(
    task: &Task,
    module_directory: &RelativeDirectory,
    context: &BuildContext,
    config: &ExecutionConfig,
    runtime: &impl Runtime,
) -> MietteResult<(TaskOutcome, AbsoluteDirectory)> {
    // `generate-go-work` is the only task run this way, and it declares no parameters.
    let no_parameters: ParameterState = ParameterState::default();
    let module: ModuleContext = ModuleContext::new(module_directory, &no_parameters);
    let plan: TaskPlan = resolve_task_plan(task, &module, module_directory, context, runtime)?;
    let output_directory: AbsoluteDirectory = plan.layout.output_directory().clone();
    let outcome: TaskOutcome = match plan.dirtiness {
        Dirtiness::Clean => TaskOutcome::cached(plan.task, TaskStart::new(runtime.now()), Fiber::new(0)),
        Dirtiness::Dirty => {
            if config.verbosity != Verbosity::Quiet {
                writeln!(runtime.output(), "  \u{2192} {}", plan.task.name()).into_diagnostic()?;
            }
            run_task_and_persist(
                plan.task,
                &module,
                context,
                &plan.layout,
                TaskStart::new(runtime.now()),
                Fiber::new(0),
                runtime,
            )
        }
    };
    report_outcome(&outcome, config, runtime)?;
    if !outcome.output().status().is_success() {
        return Err(SindriError::TaskFailed {
            task_name: outcome.task().name().clone(),
            command: outcome.failed_command().map_or_else(
                || "no command ran (the task failed before one could)".to_string(),
                Command::to_string,
            ),
            output: outcome.output().combined_output(),
        }
        .into());
    }
    Ok((outcome, output_directory))
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

    /// No test in this module exercises a managed input, so any distinct absolute directory works.
    fn managed_input_base(working_directory: &AbsoluteDirectory) -> AbsoluteDirectory {
        working_directory.join_directory(&RelativeDirectory::new_unchecked(".target/generate-go-work"))
    }

    fn metadata_cache(build_directory: &AbsoluteDirectory) -> MetadataCache {
        MetadataCache::new(build_directory.join_directory(&RelativeDirectory::new_unchecked(".metadata-cache")))
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
        let build_directory: AbsoluteDirectory =
            working_directory.join_directory(&RelativeDirectory::new_unchecked(".target"));
        let location: ModuleLocation = ModuleLocation::new(
            working_directory.clone(),
            ModulePath::new(RelativeDirectory::new_unchecked("")),
        );
        let managed_input_base: AbsoluteDirectory = managed_input_base(working_directory);
        let cache: MetadataCache = metadata_cache(&build_directory);
        let context: BuildContext = BuildContext::new(&workspace_root, &build_directory, &managed_input_base, &cache);
        execute_graph(
            graph,
            &location,
            &ParameterState::default(),
            &context,
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
        let build_directory: AbsoluteDirectory =
            working_directory.join_directory(&RelativeDirectory::new_unchecked(".target"));
        let location: ModuleLocation = ModuleLocation::new(
            working_directory.clone(),
            ModulePath::new(RelativeDirectory::new_unchecked("")),
        );
        let managed_input_base: AbsoluteDirectory = managed_input_base(&working_directory);
        let cache: MetadataCache = metadata_cache(&build_directory);
        let context: BuildContext = BuildContext::new(&workspace_root, &build_directory, &managed_input_base, &cache);
        execute_graph(
            graph,
            &location,
            &ParameterState::default(),
            &context,
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
        let build_directory: AbsoluteDirectory =
            working_directory.join_directory(&RelativeDirectory::new_unchecked(".target"));
        let location: ModuleLocation = ModuleLocation::new(
            working_directory.clone(),
            ModulePath::new(RelativeDirectory::new_unchecked("")),
        );
        let managed_input_base: AbsoluteDirectory = managed_input_base(working_directory);
        let cache: MetadataCache = metadata_cache(&build_directory);
        let context: BuildContext = BuildContext::new(&workspace_root, &build_directory, &managed_input_base, &cache);
        execute_graph(
            graph,
            &location,
            &ParameterState::default(),
            &context,
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
