use crate::glob::GlobPatterns;
use crate::hash::DeclarationHash;
use crate::hash::FileHash;
use crate::hash::FileSetHash;
use crate::plugin::Task;
use crate::plugin::TaskName;
use crate::runtime::FileSystem;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::ModulePath;
use crate::types::RelativeDirectory;
use crate::types::RelativeFile;
use crate::types::Step;
use crate::types::WorkspaceRoot;
use serde::Deserialize;
use serde::Serialize;
use std::io::Error as IoError;
use std::io::Result as IoResult;
use std::path::Path;
use std::path::PathBuf;

/// Whether a task's persisted state matched its current inputs, outputs, and declaration. A `Hit`
/// task is skipped; a `Miss` task runs. Also drives the telemetry `cache` annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheStatus {
    Hit,
    Miss,
}

impl CacheStatus {
    /// The Chrome-trace `args.cache` value for this status.
    pub fn label(&self) -> &'static str {
        match self {
            CacheStatus::Hit => "hit",
            CacheStatus::Miss => "miss",
        }
    }
}

/// The on-disk locations a single task owns under the build directory:
/// `.target/<module-path>/<step>/<task>/`, where `<module-path>` is the module's own subtree (empty for
/// the workspace-root module, so its tasks sit directly under `.target/<step>/<task>/`). Holds its
/// `state.bin` and an `output/` subdirectory for its artifacts. Each task owns a disjoint directory, so
/// tasks — even across concurrent modules — never contend.
pub struct TaskPaths {
    state_file: AbsoluteFile,
    output_directory: AbsoluteDirectory,
}

impl TaskPaths {
    pub fn new(
        build_directory: &AbsoluteDirectory,
        module_path: &ModulePath,
        step: &Step,
        task: &TaskName,
    ) -> TaskPaths {
        let mut relative_path: PathBuf = module_path.as_relative_directory().as_ref().to_path_buf();
        relative_path.push(step.as_ref());
        relative_path.push(task.as_ref());
        let relative: RelativeDirectory = RelativeDirectory::new(relative_path);
        let task_directory: AbsoluteDirectory = build_directory.join_directory(&relative);
        TaskPaths {
            state_file: task_directory.join_file(&RelativeFile::new("state.bin")),
            output_directory: task_directory.join_directory(&RelativeDirectory::new("output")),
        }
    }

    pub fn state_file(&self) -> &AbsoluteFile {
        &self.state_file
    }

    pub fn output_directory(&self) -> &AbsoluteDirectory {
        &self.output_directory
    }
}

/// A task's recorded state: the set hashes of its inputs and outputs and its declaration hash. The
/// next build re-derives the same three values and compares; any difference re-runs the task and the
/// record is replaced wholesale.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskState {
    inputs: FileSetHash,
    outputs: FileSetHash,
    declaration: DeclarationHash,
}

impl TaskState {
    pub fn new(inputs: FileSetHash, outputs: FileSetHash, declaration: DeclarationHash) -> TaskState {
        TaskState {
            inputs,
            outputs,
            declaration,
        }
    }

    /// Derive a task's current state by hashing its input and output file sets and its declaration.
    /// Inputs are matched against the module directory, outputs against the task's output directory;
    /// a directory that does not yet exist contributes an empty set.
    pub fn compute(
        task: &Task,
        working_directory: &AbsoluteDirectory,
        output_directory: &AbsoluteDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> IoResult<TaskState> {
        Ok(TaskState {
            inputs: file_set_hash(working_directory, task.inputs(), workspace_root, file_system)?,
            outputs: file_set_hash(output_directory, task.outputs(), workspace_root, file_system)?,
            declaration: DeclarationHash::of(task.command(), task.inputs(), task.outputs()),
        })
    }

    pub fn inputs(&self) -> FileSetHash {
        self.inputs
    }

    pub fn declaration(&self) -> DeclarationHash {
        self.declaration
    }

    /// Load a task's persisted state. A missing or unreadable file (including one written by an
    /// older, incompatible format) is treated as absent, which makes the task dirty.
    pub fn load(path: &AbsoluteFile, file_system: &impl FileSystem) -> Option<TaskState> {
        let bytes: Vec<u8> = file_system.read(path.as_ref()).ok()?;
        rmp_serde::from_slice(&bytes).ok()
    }

    /// Persist this state to `path`, creating the task's directory if needed.
    pub fn persist(&self, path: &AbsoluteFile, file_system: &impl FileSystem) -> IoResult<()> {
        let bytes: Vec<u8> = rmp_serde::to_vec(self).map_err(IoError::other)?;
        if let Some(parent) = path.parent() {
            file_system.create_directories(parent.as_ref())?;
        }
        file_system.write(path.as_ref(), &bytes)
    }
}

/// Compare a task's freshly computed state against what was persisted. A task is clean only when a
/// record exists and all three hashes match; everything else (no record, changed input set, changed
/// output set, or a changed declaration) is a miss.
pub fn dirtiness(current: &TaskState, persisted: Option<&TaskState>) -> CacheStatus {
    match persisted {
        Some(previous) if previous == current => CacheStatus::Hit,
        _ => CacheStatus::Miss,
    }
}

/// Hash every file beneath `base` that `patterns` selects, into a single set hash. Each file
/// contributes its workspace-relative `/`-separated path and its content [`FileHash`]; a `base` that
/// does not exist contributes the empty set.
pub fn file_set_hash(
    base: &AbsoluteDirectory,
    patterns: &GlobPatterns,
    workspace_root: &WorkspaceRoot,
    file_system: &impl FileSystem,
) -> IoResult<FileSetHash> {
    if file_system.file_kind(base.as_ref())?.is_none() {
        return Ok(FileSetHash::of(Vec::new()));
    }
    let files: Vec<PathBuf> = file_system.matching_files(base.as_ref(), patterns)?;
    let mut entries: Vec<(String, FileHash)> = Vec::with_capacity(files.len());
    for file in files {
        let bytes: Vec<u8> = file_system.read(&file)?;
        let file_hash: FileHash = FileHash::of_bytes(&bytes);
        let relative: RelativeFile = workspace_root.relativize_file(&AbsoluteFile::new(file));
        entries.push((to_slash_path(relative.as_ref()), file_hash));
    }
    Ok(FileSetHash::of(entries))
}

/// Render a path as a `/`-separated string regardless of the host separator, so a workspace checked
/// out on Windows hashes identically to one on Linux.
fn to_slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<&str>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glob::Glob;
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::types::Command;

    const WORKSPACE: &str = "/workspace";

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)))
    }

    fn working_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from(WORKSPACE))
    }

    fn output_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace/.target/default/compile/go-compile/output"))
    }

    /// A task tracking `**/*.go` inputs with no outputs — enough to exercise dirtiness off its input
    /// set and declaration.
    fn go_task() -> Task {
        Task::new(
            TaskName::new("go-compile"),
            Step::new("compile"),
            Command::new("go", ["build", "./..."]),
            GlobPatterns::new(vec![Glob::new("**/*.go")], vec![]),
            GlobPatterns::new(vec![], vec![]),
        )
    }

    fn compute(task: &Task, runtime: &DummyRuntime) -> TaskState {
        TaskState::compute(
            task,
            &working_directory(),
            &output_directory(),
            &workspace_root(),
            runtime,
        )
        .unwrap()
    }

    fn workspace_with(source: &str) -> DummyRuntimeBuilder {
        DummyRuntime::builder().file("/workspace/main.go", source)
    }

    #[test]
    fn a_task_with_no_state_file_is_dirty() {
        let runtime: DummyRuntime = workspace_with("package main").build();
        let task: Task = go_task();
        let current: TaskState = compute(&task, &runtime);
        let persisted: Option<TaskState> = TaskState::load(
            TaskPaths::new(
                &AbsoluteDirectory::new(PathBuf::from("/workspace/.target")),
                &ModulePath::new(RelativeDirectory::new("")),
                task.step(),
                task.name(),
            )
            .state_file(),
            &runtime,
        );
        assert_eq!(dirtiness(&current, persisted.as_ref()), CacheStatus::Miss);
    }

    #[test]
    fn unchanged_inputs_and_outputs_are_clean() {
        let runtime: DummyRuntime = workspace_with("package main").build();
        let task: Task = go_task();
        let recorded: TaskState = compute(&task, &runtime);
        let current: TaskState = compute(&task, &runtime);
        assert_eq!(dirtiness(&current, Some(&recorded)), CacheStatus::Hit);
    }

    #[test]
    fn a_changed_input_file_is_dirty() {
        let task: Task = go_task();
        let recorded: TaskState = compute(&task, &workspace_with("package main").build());
        let current: TaskState = compute(&task, &workspace_with("package main // edited").build());
        assert_eq!(dirtiness(&current, Some(&recorded)), CacheStatus::Miss);
    }

    #[test]
    fn a_changed_declaration_is_dirty() {
        let runtime: DummyRuntime = workspace_with("package main").build();
        let recorded: TaskState = compute(&go_task(), &runtime);
        let renamed: Task = Task::new(
            TaskName::new("go-compile"),
            Step::new("compile"),
            Command::new("go", ["vet", "./..."]),
            GlobPatterns::new(vec![Glob::new("**/*.go")], vec![]),
            GlobPatterns::new(vec![], vec![]),
        );
        let current: TaskState = compute(&renamed, &runtime);
        assert_eq!(dirtiness(&current, Some(&recorded)), CacheStatus::Miss);
    }

    #[test]
    fn a_missing_output_file_is_dirty() {
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Step::new("compile"),
            Command::new("go", ["build", "-o", "{output}/", "./..."]),
            GlobPatterns::new(vec![Glob::new("**/*.go")], vec![]),
            GlobPatterns::new(vec![Glob::new("**/*")], vec![]),
        );
        // Recorded with a built binary present; current has it removed.
        let with_binary: DummyRuntime = workspace_with("package main")
            .file("/workspace/.target/default/compile/go-compile/output/app", "binary")
            .build();
        let recorded: TaskState = compute(&task, &with_binary);
        let without_binary: DummyRuntime = workspace_with("package main").build();
        let current: TaskState = compute(&task, &without_binary);
        assert_eq!(dirtiness(&current, Some(&recorded)), CacheStatus::Miss);
    }

    #[test]
    fn a_changed_output_file_is_dirty() {
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Step::new("compile"),
            Command::new("go", ["build", "-o", "{output}/", "./..."]),
            GlobPatterns::new(vec![Glob::new("**/*.go")], vec![]),
            GlobPatterns::new(vec![Glob::new("**/*")], vec![]),
        );
        let original: DummyRuntime = workspace_with("package main")
            .file("/workspace/.target/default/compile/go-compile/output/app", "binary")
            .build();
        let recorded: TaskState = compute(&task, &original);
        let tampered: DummyRuntime = workspace_with("package main")
            .file("/workspace/.target/default/compile/go-compile/output/app", "tampered")
            .build();
        let current: TaskState = compute(&task, &tampered);
        assert_eq!(dirtiness(&current, Some(&recorded)), CacheStatus::Miss);
    }

    #[test]
    fn state_round_trips_through_persistence() {
        let runtime: DummyRuntime = workspace_with("package main").build();
        let task: Task = go_task();
        let recorded: TaskState = compute(&task, &runtime);
        let path: AbsoluteFile =
            AbsoluteFile::new(PathBuf::from("/workspace/.target/default/compile/go-compile/state.bin"));
        recorded.persist(&path, &runtime).unwrap();
        let written: Vec<u8> = runtime
            .written_file(path.as_ref())
            .expect("state.bin should be written");
        let reloaded: TaskState = rmp_serde::from_slice(&written).unwrap();
        assert_eq!(reloaded, recorded);
    }
}
