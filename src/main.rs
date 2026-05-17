use clap::Parser;
use sindri::error::SindriError;
use sindri::types::WorkspaceRoot;
use sindri::workspace::Workspace;
use sindri::workspace::find_root;
use sindri::workspace::load;
use std::fmt;
use std::path::PathBuf;
use std::time::Instant;
use time::OffsetDateTime;
use tracing_appender::non_blocking::NonBlocking;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::RollingFileAppender;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;

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
}

struct LocalTimeWithElapsed {
    start: Instant,
}

impl FormatTime for LocalTimeWithElapsed {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        let now: OffsetDateTime = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
        let elapsed_ms: u64 = self.start.elapsed().as_millis() as u64;
        let millis: u64 = elapsed_ms % 1000;
        let total_seconds: u64 = elapsed_ms / 1000;
        let seconds: u64 = total_seconds % 60;
        let minutes: u64 = total_seconds / 60;
        write!(
            writer,
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03} [{minutes:04}:{seconds:02}.{millis:03}]",
            now.year(),
            now.month() as u8,
            now.day(),
            now.hour(),
            now.minute(),
            now.second(),
            now.millisecond(),
        )
    }
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
        std::fs::create_dir_all(&log_directory).map_err(|source: std::io::Error| -> SindriError {
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
            .with_timer(LocalTimeWithElapsed { start })
            .init();
        Some(guard)
    } else {
        None
    };
    tracing::info!("Workspace loaded: {workspace_root}");
    Ok(())
}
