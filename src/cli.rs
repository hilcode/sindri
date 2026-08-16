use crate::error::SindriError;
use crate::executor::ExecutionConfig;
use crate::executor::Verbosity;
use crate::lifecycle::Lifecycle;
use crate::runtime::Bootstrap;
use crate::types::AbsoluteFile;
use crate::types::BuildStart;
use crate::types::RelativeFile;
use crate::workspace::Workspace;
use clap::Parser;
use clap::Subcommand;
use miette::IntoDiagnostic;
use miette::Result as MietteResult;

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
    /// Run all tasks up to and including the compile step.
    #[command(verbatim_doc_comment)]
    Compile,
    /// Remove the build directory.
    #[command(verbatim_doc_comment)]
    Clean,
}

pub fn run(start: BuildStart, arguments: Arguments, file_system: impl Bootstrap) -> MietteResult<()> {
    let workspace: Workspace = Workspace::locate(&file_system)?;
    let log_path: Option<AbsoluteFile> = if arguments.log {
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
    let lifecycle: Lifecycle = Lifecycle::default();
    let verbosity: Verbosity = if arguments.quiet {
        Verbosity::Quiet
    } else if arguments.verbose {
        Verbosity::Verbose
    } else {
        Verbosity::Normal
    };
    match arguments.action {
        Action::Lifecycle { all } => lifecycle.run_lifecycle(all, &runtime).into_diagnostic()?,
        Action::Compile => {
            let config: ExecutionConfig = ExecutionConfig::new(verbosity, start);
            lifecycle.run_compile(&workspace, &config, &runtime)?
        }
        Action::Clean => {
            let config: ExecutionConfig = ExecutionConfig::new(verbosity, start);
            lifecycle.run_clean(&workspace, &config, &runtime)?
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

    #[test]
    fn run_lifecycle_action_succeeds_on_a_valid_workspace() {
        let runtime: DummyRuntime = workspace().build();
        let arguments: Arguments = Arguments {
            log: false,
            quiet: false,
            verbose: false,
            action: Action::Lifecycle { all: true },
        };
        assert!(run(BuildStart::now(), arguments, runtime).is_ok());
    }

    #[test]
    fn run_with_the_log_flag_succeeds() {
        let runtime: DummyRuntime = workspace().build();
        let arguments: Arguments = Arguments {
            log: true,
            quiet: false,
            verbose: false,
            action: Action::Lifecycle { all: false },
        };
        assert!(run(BuildStart::now(), arguments, runtime).is_ok());
    }

    #[test]
    fn run_compile_action_runs_the_build_commands() {
        let runtime: DummyRuntime = go_workspace()
            .command("rm -f go.work", succeeded())
            .command("go work init", succeeded())
            .command("gofmt -l .", succeeded())
            .command("go build", succeeded())
            .build();
        let arguments: Arguments = Arguments {
            log: false,
            quiet: true,
            verbose: false,
            action: Action::Compile,
        };
        assert!(run(BuildStart::now(), arguments, runtime).is_ok());
    }

    #[test]
    fn run_clean_action_removes_everything_under_the_build_directory() {
        let runtime: DummyRuntime = workspace()
            .file("/workspace/.target/go-compile/binding/app", "")
            .command("rm -rf go-compile", succeeded())
            .build();
        let arguments: Arguments = Arguments {
            log: false,
            quiet: true,
            verbose: false,
            action: Action::Clean,
        };
        assert!(run(BuildStart::now(), arguments, runtime).is_ok());
    }

    #[test]
    fn run_clean_action_fails_when_the_command_fails() {
        let failed: CommandOutput = CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Failed);
        let runtime: DummyRuntime = workspace()
            .file("/workspace/.target/go-compile/binding/app", "")
            .command("rm -rf go-compile", failed)
            .build();
        let arguments: Arguments = Arguments {
            log: false,
            quiet: true,
            verbose: false,
            action: Action::Clean,
        };
        assert!(run(BuildStart::now(), arguments, runtime).is_err());
    }

    #[test]
    fn run_clean_action_fails_when_the_command_cannot_be_run() {
        // No "rm -rf go-compile" stub registered, so `run_command` itself returns an IO error,
        // which the task pipeline folds into a failed outcome — proving that failure is surfaced
        // too, not silently swallowed.
        let runtime: DummyRuntime = workspace()
            .file("/workspace/.target/go-compile/binding/app", "")
            .build();
        let arguments: Arguments = Arguments {
            log: false,
            quiet: true,
            verbose: false,
            action: Action::Clean,
        };
        let error: miette::Report = run(BuildStart::now(), arguments, runtime).unwrap_err();
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
        let arguments: Arguments = Arguments {
            log: false,
            quiet: false,
            verbose: true,
            action: Action::Clean,
        };
        assert!(run(BuildStart::now(), arguments, runtime).is_ok());
    }

    #[test]
    fn run_without_a_workspace_returns_an_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().current_directory("/nowhere").build();
        let arguments: Arguments = Arguments {
            log: false,
            quiet: false,
            verbose: false,
            action: Action::Lifecycle { all: false },
        };
        assert!(run(BuildStart::now(), arguments, runtime).is_err());
    }
}
