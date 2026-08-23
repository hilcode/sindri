use crate::error::SindriError;
use crate::error::SindriResult;
use crate::executor::ExecutionConfig;
use crate::executor::Verbosity;
use crate::lifecycle::Lifecycle;
use crate::lifecycles::Lifecycles;
use crate::plugins::PluginRegistry;
use crate::runtime::Bootstrap;
use crate::task::Task;
use crate::types::AbsoluteFile;
use crate::types::BuildStart;
use crate::types::RelativeFile;
use crate::types::Step;
use crate::workspace::Workspace;
use clap::ArgMatches;
use clap::Command;
use clap::CommandFactory;
use clap::Parser;
use clap::Subcommand;
use miette::IntoDiagnostic;
use miette::Result as MietteResult;
use std::path::PathBuf;

const LONG_ABOUT: &str = "\
Sindri is a build tool that combines the best properties of existing
build systems while avoiding their principal failure modes.

Design principles:
  Declarative — build files describe what a module is, not how to build it.
  Explicit    — dependencies, plugin ordering, and tool requirements are all declared.
  Correct     — inputs and outputs are tracked precisely; work is skipped safely.
  Recoverable — the tool can recover from any build failure without a full clean rebuild.
  Extensible  — new languages and tools are added through a typed plugin API.
  IDE-first   — Build Server Protocol (BSP) support is a core feature.\
";

#[derive(Parser)]
#[command(
    version,
    about = "A declarative, lifecycle-driven build tool with incremental correctness.",
    long_about = LONG_ABOUT
)]
pub struct Arguments {
    #[arg(long, help = "Write trace output to <build_directory>/sindri.log.")]
    log: bool,
    #[arg(
        short,
        long,
        global = true,
        help = "Suppress progress output; only errors are shown."
    )]
    quiet: bool,
    #[arg(short, long, global = true, help = "Show more detail in progress output.")]
    verbose: bool,
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Print the resolved lifecycle steps and the tasks bound to each.
    /// By default only steps with tasks are shown; use --all to see every step.
    #[command(verbatim_doc_comment)]
    Lifecycle {
        #[arg(short, long, help = "Show all steps, including those with no tasks.")]
        all: bool,
    },
}

/// The full CLI command: [`Arguments`]'s declarative shape (global flags, the `lifecycle`
/// subcommand) plus one subcommand per runnable step across every lifecycle `lifecycles` carries —
/// sourced from loaded/embedded data rather than a hardcoded enum, since the step set is only known
/// once lifecycles are loaded.
fn build_command(lifecycles: &Lifecycles) -> Command {
    let mut command: Command = Arguments::command();
    for (_, step) in lifecycles.runnable_steps() {
        command = command.subcommand(
            Command::new(step.to_string()).about(format!("Run all tasks up to and including the `{step}` step.")),
        );
    }
    command
}

/// Resolves `workspace`, erroring with the same signal [`Workspace::locate`] gives when none is
/// found. Reconstructed from the current directory rather than by locating a second time — a
/// missing workspace was already established once, before this invocation's command was even
/// parsed.
fn require_workspace(workspace: Option<Workspace>, file_system: &impl Bootstrap) -> SindriResult<Workspace> {
    match workspace {
        Some(workspace) => Ok(workspace),
        None => {
            let start: PathBuf = file_system.current_directory().map_err(|source| SindriError::Io {
                path: PathBuf::new(),
                source,
            })?;
            Err(SindriError::WorkspaceNotFound { start })
        }
    }
}

/// Runs the CLI: parses process arguments against the command built from `lifecycles`, then
/// dispatches to the matched subcommand. `workspace` and `lifecycles` are already resolved by the
/// caller — locating a workspace and loading `.sindri/lifecycles/` both happen once, before this is
/// called, since the command itself needs `lifecycles` to know which step subcommands to offer.
pub fn run(
    start: BuildStart,
    workspace: Option<Workspace>,
    lifecycles: Lifecycles,
    file_system: impl Bootstrap,
) -> MietteResult<()> {
    let matches: ArgMatches = build_command(&lifecycles).get_matches();
    dispatch(start, matches, workspace, lifecycles, file_system)
}

fn dispatch(
    start: BuildStart,
    matches: ArgMatches,
    workspace: Option<Workspace>,
    lifecycles: Lifecycles,
    file_system: impl Bootstrap,
) -> MietteResult<()> {
    let verbosity: Verbosity = if matches.get_flag("quiet") {
        Verbosity::Quiet
    } else if matches.get_flag("verbose") {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };
    let log: bool = matches.get_flag("log");
    let workspace: Workspace = require_workspace(workspace, &file_system)?;
    let log_path: Option<AbsoluteFile> = if log {
        Some(
            workspace
                .absolute_build_directory()
                .join_file(&RelativeFile::new("sindri.log").expect("a literal file name is always well-formed")),
        )
    } else {
        None
    };
    let runtime = file_system
        .into_runtime(start, log_path)
        .map_err(|source| SindriError::Io {
            path: workspace.config().build_directory().as_ref().to_path_buf(),
            source,
        })?;
    workspace.log_loaded(&runtime)?;
    let plugins: PluginRegistry = PluginRegistry::load(&workspace, &runtime)?;
    let (name, sub_matches): (&str, &ArgMatches) = matches.subcommand().expect("clap requires a subcommand");
    match name {
        "lifecycle" => {
            let all: bool = sub_matches.get_flag("all");
            lifecycles
                .default_lifecycle()
                .run_lifecycle(&plugins, all, &runtime)
                .into_diagnostic()?;
        }
        step_name => {
            let (lifecycle, step): (Lifecycle, Step) = lifecycles
                .into_step(step_name)
                .expect("clap only offers step names sourced from these lifecycles");
            let config: ExecutionConfig = ExecutionConfig::new(verbosity, start);
            let workspace_tasks: Vec<(Task, Step)> = lifecycle.workspace_tasks();
            lifecycle.run_step(&step, &workspace_tasks, &workspace, &plugins, &config, &runtime)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::types::CommandOutput;
    use crate::types::Stderr;
    use crate::types::Stdout;
    use crate::types::TaskStatus;
    use std::io::ErrorKind;

    fn succeeded() -> CommandOutput {
        CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Succeeded)
    }

    /// A runtime seeded with just a workspace root — enough for the `lifecycle` action, which reads
    /// no module.
    fn workspace() -> DummyRuntimeBuilder {
        DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .current_directory("/workspace")
    }

    /// The workspace above plus a Go module, ready for the `compile` action.
    fn go_workspace() -> DummyRuntimeBuilder {
        workspace().file(
            "/workspace/sindri.build",
            r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0",
                 parameters = { "sindri-go" = { mode = "debug" } } }"#,
        )
    }

    /// Runs the CLI exactly as `main` does: locate the (already-built) workspace's lifecycles, build
    /// the augmented command, parse `args` against it, and dispatch.
    fn run_args(args: &[&str], runtime: DummyRuntime) -> MietteResult<()> {
        let workspace: Option<Workspace> = Workspace::locate(&runtime).ok();
        let lifecycles: Lifecycles = match &workspace {
            Some(workspace) => Lifecycles::load(workspace.workspace_root(), &runtime).unwrap(),
            None => Lifecycles::embedded_defaults(),
        };
        let matches: ArgMatches = build_command(&lifecycles).try_get_matches_from(args).unwrap();
        dispatch(BuildStart::now(), matches, workspace, lifecycles, runtime)
    }

    #[test]
    fn run_lifecycle_action_succeeds_on_a_valid_workspace() {
        let runtime: DummyRuntime = workspace().build();
        assert!(run_args(&["sindri", "lifecycle", "--all"], runtime).is_ok());
    }

    #[test]
    fn run_with_the_log_flag_succeeds() {
        let runtime: DummyRuntime = workspace().build();
        assert!(run_args(&["sindri", "--log", "lifecycle"], runtime).is_ok());
    }

    #[test]
    fn run_compile_action_runs_the_build_commands() {
        let runtime: DummyRuntime = go_workspace()
            .command("rm -f go.work", succeeded())
            .command("go work init", succeeded())
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .build();
        assert!(run_args(&["sindri", "--quiet", "compile"], runtime).is_ok());
    }

    #[test]
    fn run_clean_action_removes_everything_under_the_build_directory() {
        let runtime: DummyRuntime = workspace()
            .file("/workspace/.target/go-compile/binding/app", "")
            .command("rm -rf go-compile", succeeded())
            .build();
        assert!(run_args(&["sindri", "--quiet", "clean"], runtime).is_ok());
    }

    #[test]
    fn run_clean_action_fails_when_the_command_fails() {
        let failed: CommandOutput = CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Failed);
        let runtime: DummyRuntime = workspace()
            .file("/workspace/.target/go-compile/binding/app", "")
            .command("rm -rf go-compile", failed)
            .build();
        assert!(run_args(&["sindri", "--quiet", "clean"], runtime).is_err());
    }

    #[test]
    fn run_clean_action_fails_when_the_command_cannot_be_run() {
        // No "rm -rf go-compile" stub registered, so `run_command` itself returns an IO error,
        // which the task pipeline folds into a failed outcome — proving that failure is surfaced
        // too, not silently swallowed.
        let runtime: DummyRuntime = workspace()
            .file("/workspace/.target/go-compile/binding/app", "")
            .build();
        let error: miette::Report = run_args(&["sindri", "--quiet", "clean"], runtime).unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<SindriError>(),
                Some(SindriError::TaskFailed { .. })
            ),
            "expected SindriError::TaskFailed, got {error:?}"
        );
    }

    #[test]
    fn run_clean_action_on_an_empty_build_directory_runs_a_harmless_no_op() {
        // Nothing under the build directory besides `clean`'s own state, so the derived directory
        // list is empty and the script's single `rm -rf` command carries no path arguments — a
        // no-op removal, not zero commands.
        let runtime: DummyRuntime = workspace().command("rm -rf", succeeded()).build();
        assert!(run_args(&["sindri", "--verbose", "clean"], runtime).is_ok());
    }

    #[test]
    fn run_without_a_workspace_returns_an_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().current_directory("/nowhere").build();
        assert!(run_args(&["sindri", "lifecycle"], runtime).is_err());
    }

    #[test]
    fn run_without_a_workspace_reports_an_io_error_when_the_current_directory_cannot_be_read_either() {
        // No workspace to begin with, and the current directory itself can't even be read — the
        // fallback `require_workspace` takes to name what's missing hits its own IO error, rather
        // than the `WorkspaceNotFound` it would otherwise construct.
        let runtime: DummyRuntime = DummyRuntime::builder()
            .current_directory_error(ErrorKind::PermissionDenied)
            .build();
        let error: miette::Report = run_args(&["sindri", "lifecycle"], runtime).unwrap_err();
        assert!(
            matches!(error.downcast_ref::<SindriError>(), Some(SindriError::Io { .. })),
            "expected SindriError::Io, got {error:?}"
        );
    }

    #[test]
    fn help_outside_a_workspace_lists_the_embedded_default_and_clean_steps() {
        let lifecycles: Lifecycles = Lifecycles::embedded_defaults();
        let error: clap::Error = build_command(&lifecycles)
            .try_get_matches_from(["sindri", "--help"])
            .unwrap_err();
        let help: String = error.to_string();
        for step in ["generate", "compile", "package", "publish", "clean", "lifecycle"] {
            assert!(help.contains(step), "expected `--help` to list `{step}`, got:\n{help}");
        }
    }

    #[test]
    fn help_inside_a_workspace_lists_the_same_steps_sourced_from_the_bootstrapped_lifecycles() {
        let runtime: DummyRuntime = workspace().build();
        let workspace: Workspace = Workspace::locate(&runtime).unwrap();
        let lifecycles: Lifecycles = Lifecycles::load(workspace.workspace_root(), &runtime).unwrap();
        let error: clap::Error = build_command(&lifecycles)
            .try_get_matches_from(["sindri", "--help"])
            .unwrap_err();
        let help: String = error.to_string();
        for step in ["generate", "compile", "package", "publish", "clean", "lifecycle"] {
            assert!(help.contains(step), "expected `--help` to list `{step}`, got:\n{help}");
        }
    }
}
