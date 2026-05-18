use clap::Parser;
use clap::Subcommand;
use miette::IntoDiagnostic;
use sindri::error::SindriError;
use sindri::lifecycle::Lifecycle;
use sindri::local_time_with_elapsed::LocalTimeWithElapsed;
use sindri::output::TerminalOutput;
use sindri::types::WorkspaceRoot;
use sindri::workspace::Workspace;
use sindri::workspace::find_root;
use sindri::workspace::load;
use std::io;
use std::path::PathBuf;
use std::time::Instant;
use tracing_appender::non_blocking::NonBlocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::RollingFileAppender;

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
struct Arguments {
    #[arg(long, help = "Write trace output to <build_dir>/sindri.log")]
    log: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the resolved lifecycle steps and the tasks bound to each.
    /// By default only steps with tasks are shown; use --all to see every step.
    Lifecycle {
        #[arg(short, long, help = "Show all steps, including those with no tasks")]
        all: bool,
    },
    /// Resolve and print the task graph for the compile step.
    Compile,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{:?}", error);
        std::process::exit(1);
    }
}

fn run() -> miette::Result<()> {
    let start: Instant = Instant::now();
    let arguments: Arguments = Arguments::parse();
    let current_directory: PathBuf = std::env::current_dir().expect("cannot determine working directory");
    let workspace_root: WorkspaceRoot = find_root(&current_directory)?;
    let loaded_workspace: Workspace = load(&workspace_root)?;
    let _logging_guard: Option<WorkerGuard> = if arguments.log {
        let log_directory: PathBuf = workspace_root.as_ref().join(loaded_workspace.build_dir.as_ref());
        std::fs::create_dir_all(&log_directory).map_err(|source: io::Error| -> SindriError {
            SindriError::Io {
                path: log_directory.display().to_string(),
                source,
            }
        })?;
        let file_appender: RollingFileAppender = tracing_appender::rolling::never(&log_directory, "sindri.log");
        let (non_blocking, guard): (NonBlocking, WorkerGuard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_target(false)
            .with_timer(LocalTimeWithElapsed::new(start))
            .init();
        Some(guard)
    } else {
        None
    };
    tracing::info!("Workspace loaded: {workspace_root}");
    let lifecycle: Lifecycle = Lifecycle::new();
    match arguments.command {
        Command::Lifecycle { all } => lifecycle.run_lifecycle(all, &mut TerminalOutput).into_diagnostic()?,
        Command::Compile => lifecycle.run_compile(&current_directory, &workspace_root, &mut TerminalOutput)?,
    }
    Ok(())
}
