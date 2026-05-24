use serde::Deserialize;
use serde::Serialize;
use smol_str::SmolStr;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;
use std::fs::FileType;
use std::io::Result as IoResult;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Fiber(usize);

impl Fiber {
    pub fn new(index: usize) -> Fiber {
        Fiber(index)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TaskGraphNodeId(usize);

impl TaskGraphNodeId {
    pub fn new(value: usize) -> TaskGraphNodeId {
        TaskGraphNodeId(value)
    }

    pub fn value(&self) -> usize {
        self.0
    }
}

/// The instant the build started. The fixed zero point for telemetry timestamps and for the
/// elapsed-time prefix on log lines. Distinct from a [`TaskStart`] so the two can never be swapped
/// at a call site.
#[derive(Clone, Copy, Debug)]
pub struct BuildStart(Instant);

impl BuildStart {
    pub fn now() -> BuildStart {
        BuildStart(Instant::now())
    }

    /// Time elapsed from the build start until now.
    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }

    /// Time from the build start until a task started — a telemetry timestamp offset.
    pub fn elapsed_until(&self, task_start: TaskStart) -> Duration {
        task_start.0.duration_since(self.0)
    }
}

/// The instant a single task started running. Wraps a clock reading taken from
/// [`crate::runtime::Runtime::now`]; its distance from the [`BuildStart`] is the task's telemetry
/// timestamp, and its own elapsed time is the task's run duration.
#[derive(Clone, Copy, Debug)]
pub struct TaskStart(Instant);

impl TaskStart {
    pub fn new(instant: Instant) -> TaskStart {
        TaskStart(instant)
    }

    /// Time elapsed since the task started — its run duration.
    pub fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stdout(Vec<u8>);

impl From<Vec<u8>> for Stdout {
    fn from(value: Vec<u8>) -> Self {
        Stdout(value)
    }
}

impl Stdout {
    pub fn new(bytes: Vec<u8>) -> Stdout {
        Stdout(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[cfg(test)]
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("captured test output is valid UTF-8")
    }

    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

impl Write for Stdout {
    fn write(&mut self, bytes: &[u8]) -> IoResult<usize> {
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stderr(Vec<u8>);

impl From<Vec<u8>> for Stderr {
    fn from(value: Vec<u8>) -> Self {
        Stderr(value)
    }
}

impl Stderr {
    pub fn new(bytes: Vec<u8>) -> Stderr {
        Stderr(bytes)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

impl From<FileType> for FileKind {
    fn from(file_type: FileType) -> Self {
        if file_type.is_symlink() {
            FileKind::Symlink
        } else if file_type.is_dir() {
            FileKind::Directory
        } else {
            FileKind::File
        }
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    path: PathBuf,
    kind: FileKind,
}

impl DirEntry {
    pub fn new(path: PathBuf, kind: FileKind) -> DirEntry {
        DirEntry { path, kind }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file_name(&self) -> Cow<'_, str> {
        self.path
            .file_name()
            .map(|name: &OsStr| -> Cow<'_, str> { name.to_string_lossy() })
            .unwrap_or_default()
    }

    pub fn kind(&self) -> FileKind {
        self.kind
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Succeeded,
    Failed,
}

impl From<ExitStatus> for TaskStatus {
    fn from(value: ExitStatus) -> Self {
        if value.success() {
            TaskStatus::Succeeded
        } else {
            TaskStatus::Failed
        }
    }
}

impl TaskStatus {
    pub fn is_success(&self) -> bool {
        matches!(self, TaskStatus::Succeeded)
    }
}

#[derive(Clone, Debug)]
pub struct CommandOutput {
    stdout: Stdout,
    stderr: Stderr,
    status: TaskStatus,
}

impl CommandOutput {
    pub fn new(stdout: Stdout, stderr: Stderr, status: TaskStatus) -> CommandOutput {
        CommandOutput { stdout, stderr, status }
    }

    pub fn stdout(&self) -> &Stdout {
        &self.stdout
    }

    pub fn stderr(&self) -> &Stderr {
        &self.stderr
    }

    pub fn status(&self) -> TaskStatus {
        self.status
    }

    pub fn combined_output(&self) -> String {
        let mut combined: String = String::new();
        if !self.stdout.is_empty() {
            combined.push_str(&self.stdout.to_string_lossy());
        }
        if !self.stderr.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&self.stderr.to_string_lossy());
        }
        combined
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Step(SmolStr);

impl Step {
    pub fn new(name: impl Into<SmolStr>) -> Self {
        Self(name.into())
    }
}

impl AsRef<str> for Step {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Display for Step {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

/// A task's command: a program plus its arguments, executed directly with no shell. Lives here,
/// alongside [`CommandOutput`], so [`crate::runtime`] can spawn it directly without parsing a
/// string. Holding the program separately from its arguments makes the program's presence an
/// invariant of the type and keeps an argument containing whitespace a single argument.
#[derive(Clone, Debug)]
pub struct Command {
    program: SmolStr,
    arguments: Vec<SmolStr>,
}

impl Command {
    pub fn new(program: impl Into<SmolStr>, arguments: impl IntoIterator<Item = impl Into<SmolStr>>) -> Command {
        Command {
            program: program.into(),
            arguments: arguments.into_iter().map(|argument| argument.into()).collect(),
        }
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self) -> &[SmolStr] {
        &self.arguments
    }

    /// Resolve the `{output}` placeholder in each argument to a task's output directory, yielding a
    /// ready-to-spawn command. Tasks that do not reference `{output}` are returned unchanged.
    pub fn with_output_directory(&self, output_directory: &Path) -> Command {
        let replacement: Cow<'_, str> = output_directory.to_string_lossy();
        Command {
            program: self.program.clone(),
            arguments: self
                .arguments
                .iter()
                .map(|argument: &SmolStr| -> SmolStr { SmolStr::new(argument.replace("{output}", &replacement)) })
                .collect(),
        }
    }
}

/// Renders the command as a single space-joined line for diagnostics and logs. This is a display
/// convenience only — execution always uses the structured program and arguments, so an argument
/// containing spaces is never re-split.
impl Display for Command {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.program)?;
        for argument in &self.arguments {
            write!(formatter, " {argument}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct Version(SmolStr);

impl AsRef<str> for Version {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct ModuleName(SmolStr);

impl AsRef<str> for ModuleName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceName(SmolStr);

impl AsRef<str> for WorkspaceName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct BuildDirectory(RelativeDirectory);

impl BuildDirectory {
    pub fn new(directory: RelativeDirectory) -> Self {
        Self(directory)
    }

    pub fn absolute(&self, root: &WorkspaceRoot) -> AbsoluteDirectory {
        root.to_absolute_directory().join_directory(&self.0)
    }
}

impl AsRef<Path> for BuildDirectory {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

#[derive(Debug)]
pub struct WorkingDirectory(RelativeDirectory);

impl WorkingDirectory {
    pub fn new(directory: RelativeDirectory) -> WorkingDirectory {
        WorkingDirectory(directory)
    }

    pub fn derive(absolute: &AbsoluteDirectory, root: &WorkspaceRoot) -> WorkingDirectory {
        WorkingDirectory(root.relativize_directory(absolute))
    }

    pub fn absolute(&self, root: &WorkspaceRoot) -> AbsoluteDirectory {
        root.to_absolute_directory().join_directory(&self.0)
    }
}

impl AsRef<Path> for WorkingDirectory {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

#[derive(Debug)]
pub enum Language {
    Go,
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value: SmolStr = SmolStr::deserialize(deserializer)?;
        match value.as_str() {
            "go" => Ok(Language::Go),
            other => Err(serde::de::Error::custom(format!(
                "unknown language `{other}`; expected one of: go"
            ))),
        }
    }
}

/// A path to a file, relative to some directory (ultimately a [`WorkspaceRoot`]). The "relative"
/// half of the path type-system: resolve it against an absolute base with
/// [`AbsoluteDirectory::join_file`] to obtain an [`AbsoluteFile`]. A `PathBuf` only becomes a
/// `RelativeFile` here, at the edge, so everything downstream is typed.
#[derive(Clone, Debug)]
pub struct RelativeFile(PathBuf);

impl RelativeFile {
    pub fn new(path: impl Into<PathBuf>) -> RelativeFile {
        let path: PathBuf = path.into();
        debug_assert!(
            path.is_relative(),
            "RelativeFile constructed from an absolute path: {}",
            path.display()
        );
        RelativeFile(path)
    }
}

impl AsRef<Path> for RelativeFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Display for RelativeFile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        self.0.display().fmt(formatter)
    }
}

/// A path to a directory, relative to some directory (ultimately a [`WorkspaceRoot`]). Resolve it
/// against an absolute base with [`AbsoluteDirectory::join_directory`] to obtain an
/// [`AbsoluteDirectory`].
#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct RelativeDirectory(PathBuf);

impl RelativeDirectory {
    pub fn new(path: impl Into<PathBuf>) -> RelativeDirectory {
        let path: PathBuf = path.into();
        debug_assert!(
            path.is_relative(),
            "RelativeDirectory constructed from an absolute path: {}",
            path.display()
        );
        RelativeDirectory(path)
    }
}

impl AsRef<Path> for RelativeDirectory {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// An absolute path to a directory. Only ever produced by resolving a [`RelativeDirectory`] against
/// a [`WorkspaceRoot`] — itself an absolute directory — so the "absolute" invariant holds by
/// construction rather than by convention. Used wherever a real directory location is required: a
/// process working directory, an output directory to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbsoluteDirectory(PathBuf);

impl AbsoluteDirectory {
    pub fn new(path: PathBuf) -> AbsoluteDirectory {
        debug_assert!(
            path.is_absolute(),
            "AbsoluteDirectory constructed from a relative path: {}",
            path.display()
        );
        AbsoluteDirectory(path)
    }

    /// Resolve a relative file inside this directory.
    pub fn join_file(&self, relative: &RelativeFile) -> AbsoluteFile {
        AbsoluteFile::new(self.0.join(relative))
    }

    /// Resolve a relative subdirectory of this directory.
    pub fn join_directory(&self, relative: &RelativeDirectory) -> AbsoluteDirectory {
        AbsoluteDirectory::new(self.0.join(relative))
    }

    /// The parent directory, or `None` at the filesystem root. The parent of an absolute directory
    /// is itself absolute, so the invariant is preserved when walking up a tree.
    pub fn parent(&self) -> Option<AbsoluteDirectory> {
        self.0
            .parent()
            .map(|parent: &Path| -> AbsoluteDirectory { AbsoluteDirectory::new(parent.to_path_buf()) })
    }
}

impl AsRef<Path> for AbsoluteDirectory {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

/// An absolute path to a file. Produced by resolving a relative file against an
/// [`AbsoluteDirectory`]. Used wherever a file is read or written.
#[derive(Clone, Debug)]
pub struct AbsoluteFile(PathBuf);

impl AbsoluteFile {
    pub fn new(path: PathBuf) -> AbsoluteFile {
        debug_assert!(
            path.is_absolute(),
            "AbsoluteFile constructed from a relative path: {}",
            path.display()
        );
        AbsoluteFile(path)
    }

    /// The directory containing this file, or `None` at the filesystem root. An absolute file's
    /// parent is itself an absolute directory.
    pub fn parent(&self) -> Option<AbsoluteDirectory> {
        self.0
            .parent()
            .map(|parent: &Path| -> AbsoluteDirectory { AbsoluteDirectory::new(parent.to_path_buf()) })
    }
}

impl AsRef<Path> for AbsoluteFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug)]
pub struct WorkspaceRoot(AbsoluteDirectory);

impl WorkspaceRoot {
    pub fn new(directory: AbsoluteDirectory) -> Self {
        Self(directory)
    }

    pub fn to_absolute_directory(&self) -> AbsoluteDirectory {
        self.0.clone()
    }

    /// Express an absolute file as a [`RelativeFile`] relative to this root — the inverse of
    /// [`AbsoluteDirectory::join_file`].
    pub fn relativize_file(&self, absolute: &AbsoluteFile) -> RelativeFile {
        RelativeFile::new(self.relative_path(absolute.as_ref()))
    }

    /// Express an absolute directory as a [`RelativeDirectory`] relative to this root — the inverse
    /// of [`AbsoluteDirectory::join_directory`].
    pub fn relativize_directory(&self, absolute: &AbsoluteDirectory) -> RelativeDirectory {
        RelativeDirectory::new(self.relative_path(absolute.as_ref()))
    }

    fn relative_path(&self, absolute: &Path) -> PathBuf {
        absolute
            .strip_prefix(self.0.as_ref())
            .expect("path was resolved against this workspace root, so it lies within it")
            .to_path_buf()
    }
}

impl AsRef<Path> for WorkspaceRoot {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

impl Display for WorkspaceRoot {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        self.0.as_ref().display().fmt(formatter)
    }
}

#[derive(Debug)]
pub struct BuildFile(RelativeFile);

impl BuildFile {
    pub fn new(file: RelativeFile) -> Self {
        Self(file)
    }

    pub fn absolute(&self, root: &WorkspaceRoot) -> AbsoluteFile {
        root.to_absolute_directory().join_file(&self.0)
    }

    /// Pair this build file with its on-disk location to obtain the [`ConfigFile`] the Nickel
    /// evaluator reads.
    pub fn config_file(&self, root: &WorkspaceRoot) -> ConfigFile {
        ConfigFile::resolve(self.0.clone(), root)
    }

    /// The build file's qualifier, which names this module's state subtree under the build directory.
    /// A plain `sindri.build` is the [`Qualifier::default`]; a `sindri-<name>.build` yields `<name>`.
    pub fn qualifier(&self) -> Qualifier {
        let file_name: &str = self
            .0
            .as_ref()
            .file_name()
            .and_then(|name: &OsStr| -> Option<&str> { name.to_str() })
            .unwrap_or_default();
        match file_name
            .strip_prefix("sindri-")
            .and_then(|rest: &str| -> Option<&str> { rest.strip_suffix(".build") })
        {
            Some(name) => Qualifier::new(name),
            None => Qualifier::default(),
        }
    }
}

/// Names a module's state subtree under the build directory: `default` for `sindri.build`, or the
/// qualifier name (e.g. `kotlin`) for `sindri-<name>.build`. Keeps parallel modules' state in
/// separate directories so they never contend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Qualifier(SmolStr);

impl Qualifier {
    pub fn new(name: impl Into<SmolStr>) -> Qualifier {
        Qualifier(name.into())
    }
}

impl Default for Qualifier {
    fn default() -> Qualifier {
        Qualifier(SmolStr::new_static("default"))
    }
}

impl AsRef<str> for Qualifier {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<Path> for BuildFile {
    fn as_ref(&self) -> &Path {
        self.0.as_ref()
    }
}

impl Display for BuildFile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        self.0.fmt(formatter)
    }
}

/// A Nickel configuration file (`sindri.workspace` or a `sindri.build`) to be read and evaluated,
/// carrying both paths it is known by: the `disk_path` used to read its bytes off the filesystem,
/// and the workspace-root-relative `workspace_path` used to identify it in diagnostics. These are
/// always the same file viewed two ways and always travel together, so bundling them keeps the
/// pair named and threaded as a unit instead of as two loose parameters.
#[derive(Clone, Debug)]
pub struct ConfigFile {
    disk_path: AbsoluteFile,
    workspace_path: RelativeFile,
}

impl ConfigFile {
    pub fn new(disk_path: AbsoluteFile, workspace_path: RelativeFile) -> ConfigFile {
        ConfigFile {
            disk_path,
            workspace_path,
        }
    }

    /// Resolve a workspace-relative file against its root to recover its on-disk path, pairing the
    /// two into a `ConfigFile`.
    pub fn resolve(workspace_path: RelativeFile, root: &WorkspaceRoot) -> ConfigFile {
        let disk_path: AbsoluteFile = root.to_absolute_directory().join_file(&workspace_path);
        ConfigFile::new(disk_path, workspace_path)
    }

    pub fn disk_path(&self) -> &AbsoluteFile {
        &self.disk_path
    }

    pub fn workspace_path(&self) -> &RelativeFile {
        &self.workspace_path
    }
}
