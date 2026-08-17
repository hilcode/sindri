use sindri::cli::run;
use sindri::error::SindriError;
use sindri::lifecycles::Lifecycles;
use sindri::runtime::SystemFileSystem;
use sindri::types::BuildStart;
use sindri::workspace::Workspace;
use std::process::exit;

fn main() {
    // Capture the start instant before anything else so durations are as honest as possible.
    let start: BuildStart = BuildStart::now();
    let file_system: SystemFileSystem = SystemFileSystem;
    // Locate the workspace once, before the CLI command is even built — it decides both whether
    // `.sindri/lifecycles/` can be loaded (below) and whether a later step dispatch has a workspace
    // to run against, so there is no separate re-location later.
    let workspace: Option<Workspace> = match Workspace::locate(&file_system) {
        Ok(workspace) => Some(workspace),
        Err(SindriError::WorkspaceNotFound { .. }) => None,
        Err(error) => fail(error),
    };
    let lifecycles: Lifecycles = match &workspace {
        Some(workspace) => {
            Lifecycles::load(workspace.workspace_root(), &file_system).unwrap_or_else(|error| fail(error))
        }
        None => Lifecycles::embedded_defaults(),
    };
    if let Err(error) = run(start, workspace, lifecycles, file_system) {
        fail(error);
    }
}

fn fail(error: impl Into<miette::Report>) -> ! {
    eprintln!("{:?}", error.into());
    exit(1);
}
