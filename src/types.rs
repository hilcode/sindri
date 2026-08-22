use serde::Deserialize;
use serde::Serialize;
use serde::de::Error as DeserializeError;
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
use std::time::SystemTime;

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

/// A file's cheaply obtained metadata — its size and modification time, from a `stat`-equivalent
/// call rather than a read of its content. Cheap enough to check before falling back to a full
/// content hash when deciding whether a file might have changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileMetadata {
    size: u64,
    modified: SystemTime,
}

impl FileMetadata {
    pub fn new(size: u64, modified: SystemTime) -> FileMetadata {
        FileMetadata { size, modified }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn modified(&self) -> SystemTime {
        self.modified
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
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

impl<'deserialize> Deserialize<'deserialize> for Language {
    fn deserialize<Deserializer: serde::Deserializer<'deserialize>>(
        deserializer: Deserializer,
    ) -> Result<Self, Deserializer::Error> {
        let value: SmolStr = SmolStr::deserialize(deserializer)?;
        match value.as_str() {
            "go" => Ok(Language::Go),
            other => Err(DeserializeError::custom(format!(
                "unknown language `{other}`; expected one of: go"
            ))),
        }
    }
}

/// Why a string failed to become a [`RelativeFile`] or [`RelativeDirectory`]: absolute rather than
/// relative, a repeated `/` somewhere in the path, or (directory-specific) missing the required
/// trailing `/`, or (file-specific) carrying one it must not. Carries the fully rendered message so
/// both `new`'s callers and its `Deserialize` delegate surface identical text.
#[derive(Clone, Debug)]
pub struct InvalidRelativePath {
    message: String,
}

impl InvalidRelativePath {
    fn new(message: String) -> InvalidRelativePath {
        InvalidRelativePath { message }
    }
}

impl Display for InvalidRelativePath {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for InvalidRelativePath {}

/// A path to a file, relative to some directory (ultimately a [`WorkspaceRoot`]). The "relative"
/// half of the path type-system: resolve it against an absolute base with
/// [`AbsoluteDirectory::join_file`] to obtain an [`AbsoluteFile`]. A `PathBuf` only becomes a
/// `RelativeFile` here, at the edge, so everything downstream is typed.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct RelativeFile(PathBuf);

impl RelativeFile {
    /// Validate `path` as a relative file: not absolute, no repeated `/` anywhere, and not
    /// directory-like (must not end in `/`). The single validating constructor — [`Deserialize`]
    /// delegates to it directly, so a Nickel-authored file string and a Rust-constructed one are
    /// held to exactly the same rule.
    pub fn new(path: impl Into<PathBuf>) -> Result<RelativeFile, InvalidRelativePath> {
        let path: PathBuf = path.into();
        let text: Cow<'_, str> = path.to_string_lossy();
        if !path.is_relative() {
            Err(InvalidRelativePath::new(format!(
                "expected a relative file, got an absolute path `{}`",
                path.display()
            )))
        } else if text.contains("//") {
            Err(InvalidRelativePath::new(format!(
                "expected a relative file with no repeated `/`, got `{}`",
                path.display()
            )))
        } else if text.ends_with('/') {
            Err(InvalidRelativePath::new(format!(
                "expected a relative file, got a directory-like path ending in `/`: `{}`",
                path.display()
            )))
        } else {
            Ok(RelativeFile(path))
        }
    }

    /// Construct a [`RelativeFile`] from a trusted, statically-known-good literal, skipping
    /// [`new`](RelativeFile::new)'s validation — test-only, for fixture data too tedious to
    /// `.expect()` at every call site.
    #[cfg(test)]
    pub fn new_unchecked(path: impl Into<PathBuf>) -> RelativeFile {
        let path: PathBuf = path.into();
        debug_assert!(
            path.is_relative(),
            "RelativeFile constructed from an absolute path: {}",
            path.display()
        );
        RelativeFile(path)
    }
}

impl<'deserialize> Deserialize<'deserialize> for RelativeFile {
    fn deserialize<Deserializer: serde::Deserializer<'deserialize>>(
        deserializer: Deserializer,
    ) -> Result<Self, Deserializer::Error> {
        let path: PathBuf = PathBuf::deserialize(deserializer)?;
        RelativeFile::new(path).map_err(DeserializeError::custom)
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
/// [`AbsoluteDirectory`]. A named directory always renders with a trailing `/` — the authoring
/// convention that lets a Nickel-facing directory string read as unambiguously a directory, as
/// opposed to a [`RelativeFile`]'s bare name, which must never carry one. [`Dot`](RelativeDirectory::Dot)
/// is a distinct variant (not the empty string wearing a trailing slash) precisely because "no
/// subdirectory" has no sensible trailing-slash rendering of its own — it must join as the
/// identity, not as `/`, which would make an absolute path out of whatever it's joined against.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum RelativeDirectory {
    /// The current directory — no subdirectory relative to whatever base this resolves against,
    /// e.g. the workspace-root module's own directory. Joining against this leaves the base
    /// unchanged.
    Dot,
    /// A named subdirectory. Always carries exactly one trailing `/`, established once here by
    /// [`RelativeDirectory::new`] rather than left to every call site or renderer to get right.
    Named(PathBuf),
}

/// Bridge a computed (not Nickel-authored) path into [`RelativeDirectory::new`]'s strict contract:
/// unchanged if empty or already ending in `/`, otherwise with exactly one `/` appended. Every call
/// site using this is deriving a directory from already-typed components — a `strip_prefix` result,
/// a `parent()`, a qualifier pushed onto a directory — so the value is always well-formed already,
/// just not yet expressed with the trailing slash the constructor requires.
fn as_directory_text(path: &Path) -> String {
    let text: Cow<'_, str> = path.to_string_lossy();
    if text.is_empty() || text.ends_with('/') {
        text.into_owned()
    } else {
        format!("{text}/")
    }
}

impl RelativeDirectory {
    /// Validate `path` as a relative directory: not absolute, no repeated `/` anywhere, and
    /// (unless it names the current directory) ending in exactly one `/`. The single validating
    /// constructor — [`Deserialize`] delegates to it directly, so a Nickel-authored directory
    /// string and a Rust-constructed one are held to exactly the same rule.
    pub fn new(path: impl Into<PathBuf>) -> Result<RelativeDirectory, InvalidRelativePath> {
        let path: PathBuf = path.into();
        let text: Cow<'_, str> = path.to_string_lossy();
        if !path.is_relative() {
            Err(InvalidRelativePath::new(format!(
                "expected a relative directory, got an absolute path `{}`",
                path.display()
            )))
        } else if text.contains("//") {
            Err(InvalidRelativePath::new(format!(
                "expected a relative directory with no repeated `/`, got `{}`",
                path.display()
            )))
        } else if text.is_empty() {
            Ok(RelativeDirectory::Dot)
        } else if !text.ends_with('/') {
            Err(InvalidRelativePath::new(format!(
                "expected a relative directory ending in `/`, got `{}`",
                path.display()
            )))
        } else {
            Ok(RelativeDirectory::Named(path))
        }
    }

    /// Construct a [`RelativeDirectory`] from a trusted, statically-known-good literal, skipping
    /// [`new`](RelativeDirectory::new)'s validation and normalizing a missing or doubled trailing
    /// slash instead of rejecting it — test-only, for fixture data too tedious to `.expect()` at
    /// every call site.
    #[cfg(test)]
    pub fn new_unchecked(path: impl Into<PathBuf>) -> RelativeDirectory {
        let path: PathBuf = path.into();
        debug_assert!(
            path.is_relative(),
            "RelativeDirectory constructed from an absolute path: {}",
            path.display()
        );
        let text: Cow<'_, str> = path.to_string_lossy();
        if text.is_empty() {
            RelativeDirectory::Dot
        } else if text.ends_with('/') && !text.ends_with("//") {
            RelativeDirectory::Named(path)
        } else {
            RelativeDirectory::Named(PathBuf::from(format!("{}/", text.trim_end_matches('/'))))
        }
    }
}

impl<'deserialize> Deserialize<'deserialize> for RelativeDirectory {
    fn deserialize<Deserializer: serde::Deserializer<'deserialize>>(
        deserializer: Deserializer,
    ) -> Result<Self, Deserializer::Error> {
        let path: PathBuf = PathBuf::deserialize(deserializer)?;
        RelativeDirectory::new(path).map_err(DeserializeError::custom)
    }
}

impl AsRef<Path> for RelativeDirectory {
    fn as_ref(&self) -> &Path {
        match self {
            RelativeDirectory::Dot => Path::new(""),
            RelativeDirectory::Named(path) => path.as_path(),
        }
    }
}

impl Display for RelativeDirectory {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            RelativeDirectory::Dot => Ok(()),
            RelativeDirectory::Named(path) => path.display().fmt(formatter),
        }
    }
}

/// An absolute path to a directory. Only ever produced by resolving a [`RelativeDirectory`] against
/// a [`WorkspaceRoot`] — itself an absolute directory — so the "absolute" invariant holds by
/// construction rather than by convention. Used wherever a real directory location is required: a
/// process working directory, an output directory to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbsoluteDirectory(PathBuf);

impl AbsoluteDirectory {
    /// Never a Nickel-authored value — always resolved from a real filesystem location or joined
    /// from already-typed components — so unlike [`RelativeDirectory`] this stays infallible and
    /// simply normalizes to exactly one trailing `/` rather than rejecting a caller that omitted or
    /// doubled it.
    pub fn new(path: PathBuf) -> AbsoluteDirectory {
        debug_assert!(
            path.is_absolute(),
            "AbsoluteDirectory constructed from a relative path: {}",
            path.display()
        );
        let text: Cow<'_, str> = path.to_string_lossy();
        if text.ends_with('/') && !text.ends_with("//") {
            AbsoluteDirectory(path)
        } else {
            AbsoluteDirectory(PathBuf::from(format!("{}/", text.trim_end_matches('/'))))
        }
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

impl Display for AbsoluteDirectory {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        self.0.display().fmt(formatter)
    }
}

/// An absolute path to a file. Produced by resolving a relative file against an
/// [`AbsoluteDirectory`]. Used wherever a file is read or written.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

impl Display for AbsoluteFile {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        self.0.display().fmt(formatter)
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
            .expect("path was resolved against this workspace root, so it lies within it")
    }

    /// Express an absolute directory as a [`RelativeDirectory`] relative to this root — the inverse
    /// of [`AbsoluteDirectory::join_directory`].
    pub fn relativize_directory(&self, absolute: &AbsoluteDirectory) -> RelativeDirectory {
        let relative: PathBuf = self.relative_path(absolute.as_ref());
        RelativeDirectory::new(as_directory_text(&relative))
            .expect("path was resolved against this workspace root, so it lies within it")
    }

    /// An absolute file's path relative to this root, as an owned [`PathBuf`] — the form a
    /// path-bearing error variant carries, without the caller needing to go through
    /// [`RelativeFile`]'s `AsRef<Path>` itself.
    pub fn relative_path_buf(&self, absolute: &AbsoluteFile) -> PathBuf {
        self.relative_path(absolute.as_ref())
    }

    /// An absolute directory's path relative to this root, as an owned [`PathBuf`] — the
    /// directory counterpart to [`WorkspaceRoot::relative_path_buf`].
    pub fn relative_directory_path_buf(&self, absolute: &AbsoluteDirectory) -> PathBuf {
        self.relative_path(absolute.as_ref())
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
        self.0.fmt(formatter)
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

    /// The workspace-relative identity of the module this file defines: its parent directory paired
    /// with its qualifier. A `sindri.build` at the workspace root yields the empty-directory identity
    /// (`//`); a `libs/common/sindri-kotlin.build` yields `//libs/common [kotlin]`. This is the inverse
    /// of [`ModuleIdentity::to_build_file`] and is how the entry module joins the module graph keyed by
    /// the same identity form its dependents would name it with.
    pub fn identity(&self) -> ModuleIdentity {
        let parent: PathBuf = self.0.as_ref().parent().map(Path::to_path_buf).unwrap_or_default();
        let directory: RelativeDirectory = RelativeDirectory::new(as_directory_text(&parent))
            .expect("a build file's parent directory is always well-formed");
        ModuleIdentity::new(directory, self.qualifier())
    }

    /// The build file's qualifier, or `None` for a plain `sindri.build`; a `sindri-<name>.build`
    /// yields `Some(<name>)`. This names the module's state subtree under the build directory.
    pub fn qualifier(&self) -> Option<Qualifier> {
        let file_name: &str = self
            .0
            .as_ref()
            .file_name()
            .and_then(|name: &OsStr| -> Option<&str> { name.to_str() })
            .unwrap_or_default();
        file_name
            .strip_prefix("sindri-")
            .and_then(|rest: &str| -> Option<&str> { rest.strip_suffix(".build") })
            .map(Qualifier::new)
    }
}

/// A module qualifier: the `<name>` in a `sindri-<name>.build` file (e.g. `kotlin`), naming that
/// module's state subtree under the build directory. A plain `sindri.build` has no qualifier — that
/// "no qualifier" case is modelled as `None` at the use sites, never as a sentinel value here, so a
/// `Qualifier` always holds a genuine, user-written name. Qualifiers are case-insensitive and stored
/// in lower case (see [`Qualifier::new`]).
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Qualifier(SmolStr);

impl Qualifier {
    /// Qualifiers are case-insensitive, so the name is canonicalized to lower case: `Kotlin`,
    /// `kotlin`, and `KOTLIN` all yield the same qualifier and resolve to `sindri-kotlin.build`.
    pub fn new(name: impl Into<SmolStr>) -> Qualifier {
        let name: SmolStr = name.into();
        Qualifier(SmolStr::new(name.to_ascii_lowercase()))
    }
}

impl AsRef<str> for Qualifier {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A workspace-relative module identity, written `//libs/common` or `//libs/common [kotlin]`. It is a
/// logical identifier, not a filesystem path: the leading segment names the module's directory
/// relative to the workspace root, and the optional bracketed qualifier selects a
/// `sindri-<qualifier>.build` file in that directory (its absence selects the plain `sindri.build`).
/// This is the single form in which one module names another in a `dependencies` declaration, and the
/// key under which a loaded module is identified in the module graph.
///
/// The directory is case-sensitive — `//common` resolves to `common` and `//Common` to `Common`,
/// matching the case-sensitive filesystem — but the qualifier is case-insensitive and canonicalized to
/// lower case, so `[Kotlin]`, `[kotlin]`, and `[KOTLIN]` all name the `kotlin` qualifier and its
/// `sindri-kotlin.build` file. A workspace may still not contain two modules whose directories differ
/// only in case; that collision is rejected when the module graph is loaded.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ModuleIdentity {
    directory: RelativeDirectory,
    qualifier: Option<Qualifier>,
}

impl ModuleIdentity {
    /// Build an identity directly from an already-typed directory and optional qualifier. Unlike
    /// [`parse`](ModuleIdentity::parse), which validates surface syntax, this trusts its typed inputs
    /// — it is how an entry module derives its own identity from the [`BuildFile`] it was found at,
    /// including the workspace-root module whose directory is empty (a form [`parse`](ModuleIdentity::parse)
    /// deliberately rejects, since no other module can name it).
    pub fn new(directory: RelativeDirectory, qualifier: Option<Qualifier>) -> ModuleIdentity {
        ModuleIdentity { directory, qualifier }
    }

    /// Parse the `//<directory> [<qualifier>]` surface syntax. Directory segments are drawn from
    /// `[A-Za-z0-9._-]` (and are never the traversal names `.` or `..`); the qualifier begins with a
    /// letter and otherwise draws from `[A-Za-z0-9]` plus `-_.+#=@!~$%^&`, never ending in `.` (a
    /// trailing dot is not a legal directory name on Windows). `[`, `]`, and spaces are in neither set,
    /// so they are always structural delimiters and never part of a name — every legal identity string
    /// therefore has exactly one parse, and a directory whose real name contains those characters is
    /// simply not a valid module directory. The qualifier suffix is one or more spaces then `[name]`,
    /// so qualifiers can be aligned across lines. The directory is matched case-sensitively; the
    /// qualifier is case-insensitive and canonicalized to lower case. Any string outside this grammar
    /// yields a [`ModuleIdentityParseError`].
    pub fn parse(text: &str) -> Result<ModuleIdentity, ModuleIdentityParseError> {
        let malformed = || -> ModuleIdentityParseError {
            ModuleIdentityParseError {
                text: SmolStr::new(text),
            }
        };
        let body: &str = text.strip_prefix("//").ok_or_else(malformed)?;
        let (directory_text, qualifier): (&str, Option<Qualifier>) = match body.strip_suffix(']') {
            Some(head) => {
                // Brackets are illegal in names, so the sole `[` opens the qualifier; it is set off
                // from the directory by one or more spaces, which lets qualifiers be aligned.
                let (before_bracket, qualifier_name): (&str, &str) = head.split_once('[').ok_or_else(malformed)?;
                if !before_bracket.ends_with(' ') || !Self::is_valid_qualifier(qualifier_name) {
                    return Err(malformed());
                }
                (
                    before_bracket.trim_end_matches(' '),
                    Some(Qualifier::new(qualifier_name)),
                )
            }
            None => (body, None),
        };
        if !Self::is_valid_directory(directory_text) {
            return Err(malformed());
        }
        let directory_text: &str = directory_text.strip_suffix('/').unwrap_or(directory_text);
        Ok(ModuleIdentity {
            directory: RelativeDirectory::new(format!("{directory_text}/")).expect("validated by is_valid_directory"),
            qualifier,
        })
    }

    /// Resolve this identity to the build file that defines it, relative to the workspace root:
    /// `//libs/common` → `libs/common/sindri.build`, and `//tools/codegen [bin]` →
    /// `tools/codegen/sindri-bin.build`.
    pub fn to_build_file(&self) -> BuildFile {
        let file_name: String = match &self.qualifier {
            Some(qualifier) => format!("sindri-{}.build", qualifier.as_ref()),
            None => "sindri.build".to_string(),
        };
        BuildFile::new(
            RelativeFile::new(self.directory.as_ref().join(file_name))
                .expect("a directory joined with a plain file name is always well-formed"),
        )
    }

    /// The module's directory, relative to the workspace root — where its sources live and where its
    /// build commands run. The workspace-root module has the empty directory.
    pub fn directory(&self) -> &RelativeDirectory {
        &self.directory
    }

    /// This module's state subtree under the build directory: its directory, then the qualifier as a
    /// subdirectory when it has one. Distinct identities map to distinct paths, so no two modules share
    /// a `.target` subtree — the reason state is keyed here rather than on the qualifier alone.
    pub fn module_path(&self) -> ModulePath {
        let mut path: PathBuf = self.directory.as_ref().to_path_buf();
        if let Some(qualifier) = &self.qualifier {
            path.push(qualifier.as_ref());
        }
        ModulePath(
            RelativeDirectory::new(as_directory_text(&path))
                .expect("a directory optionally joined with a qualifier is always well-formed"),
        )
    }

    fn is_valid_segment(segment: &str) -> bool {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }

    /// Tolerates at most one trailing `/` (so `Display`'s own canonical, slash-terminated output —
    /// `RelativeDirectory::Named` always renders one — parses back to the same identity), but no
    /// more: a doubled trailing slash still yields a trailing empty segment, which `is_valid_segment`
    /// rejects.
    fn is_valid_directory(directory: &str) -> bool {
        let directory: &str = directory.strip_suffix('/').unwrap_or(directory);
        !directory.is_empty() && directory.split('/').all(Self::is_valid_segment)
    }

    fn is_valid_qualifier(qualifier: &str) -> bool {
        // Starts with a letter (so it can never be `.`/`..` or begin with a digit) and otherwise draws
        // from alphanumerics plus a set of punctuation — but never ends in `.`, since a trailing dot is
        // an illegal directory name on Windows and the qualifier becomes a directory under the build
        // tree.
        let first_is_letter: bool = qualifier
            .bytes()
            .next()
            .is_some_and(|byte: u8| byte.is_ascii_alphabetic());
        let every_byte_allowed: bool = qualifier.bytes().all(|byte: u8| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'-' | b'_' | b'.' | b'+' | b'#' | b'=' | b'@' | b'!' | b'~' | b'$' | b'%' | b'^' | b'&'
                )
        });
        first_is_letter && every_byte_allowed && !qualifier.ends_with('.')
    }
}

/// The reason a string could not be read as a [`ModuleIdentity`]. Carries the offending text so the
/// message can point at exactly what was written.
#[derive(Clone, Debug)]
pub struct ModuleIdentityParseError {
    text: SmolStr,
}

impl Display for ModuleIdentityParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        write!(
            formatter,
            "invalid module identity `{}`; expected `//path` or `//path [qualifier]`",
            self.text
        )
    }
}

impl std::error::Error for ModuleIdentityParseError {}

/// A module's state subtree under the build directory, keyed by identity so two modules never collide
/// there — a second plain `sindri.build` in another directory would share a qualifier-only key. Built
/// from a [`ModuleIdentity`] via [`ModuleIdentity::module_path`]; the workspace-root module maps to the
/// empty path, so its state sits directly under the build directory as before multi-module support.
#[derive(Clone, Debug)]
pub struct ModulePath(RelativeDirectory);

impl ModulePath {
    pub fn new(directory: RelativeDirectory) -> ModulePath {
        ModulePath(directory)
    }

    pub fn as_relative_directory(&self) -> &RelativeDirectory {
        &self.0
    }
}

/// A dependency cycle among modules, held as the ordered path that closes back on itself — the first
/// module repeated as the last, so `//a → //b → //a` records both which modules form the loop and the
/// order the edges were followed. It gives the diagnostic's arrow-chain rendering a typed home; the
/// path is kept structured (rather than pre-joined) so the closing edge is explicit.
#[derive(Clone, Debug)]
pub struct ModuleCycle(Vec<ModuleIdentity>);

impl ModuleCycle {
    pub fn new(path: Vec<ModuleIdentity>) -> ModuleCycle {
        ModuleCycle(path)
    }
}

impl Display for ModuleCycle {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        for (index, identity) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(" → ")?;
            }
            write!(formatter, "{identity}")?;
        }
        Ok(())
    }
}

impl<'deserialize> Deserialize<'deserialize> for ModuleIdentity {
    fn deserialize<Deserializer: serde::Deserializer<'deserialize>>(
        deserializer: Deserializer,
    ) -> Result<Self, Deserializer::Error> {
        let text: SmolStr = SmolStr::deserialize(deserializer)?;
        ModuleIdentity::parse(&text).map_err(|error: ModuleIdentityParseError| DeserializeError::custom(error))
    }
}

impl Display for ModuleIdentity {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        write!(formatter, "//{}", self.directory)?;
        if let Some(qualifier) = &self.qualifier {
            write!(formatter, " [{}]", qualifier.as_ref())?;
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::Write;
    use std::path::Path;
    use std::path::PathBuf;

    #[test]
    fn module_identity_resolves_default_build_file() {
        let identity: ModuleIdentity = ModuleIdentity::parse("//libs/common").unwrap();
        assert_eq!(identity.to_build_file().as_ref(), Path::new("libs/common/sindri.build"));
    }

    #[test]
    fn module_identity_resolves_qualified_build_file() {
        let identity: ModuleIdentity = ModuleIdentity::parse("//tools/codegen [bin]").unwrap();
        assert_eq!(
            identity.to_build_file().as_ref(),
            Path::new("tools/codegen/sindri-bin.build")
        );
    }

    #[test]
    fn module_identity_module_path_is_the_directory_then_the_qualifier() {
        // A plain module's state subtree is just its directory; a qualified one appends the qualifier,
        // so two modules in the same directory never share a subtree.
        let plain: ModuleIdentity = ModuleIdentity::parse("//libs/common").unwrap();
        assert_eq!(
            plain.module_path().as_relative_directory().as_ref(),
            Path::new("libs/common/")
        );
        let qualified: ModuleIdentity = ModuleIdentity::parse("//libs/common [kotlin]").unwrap();
        assert_eq!(
            qualified.module_path().as_relative_directory().as_ref(),
            Path::new("libs/common/kotlin/")
        );
        // The workspace-root module maps to the empty path, so its state sits directly under the
        // build directory.
        let root: ModuleIdentity = ModuleIdentity::new(RelativeDirectory::new_unchecked(""), None);
        assert_eq!(root.module_path().as_relative_directory().as_ref(), Path::new(""));
    }

    #[test]
    fn module_identity_directory_is_case_sensitive() {
        assert_ne!(
            ModuleIdentity::parse("//libs/common").unwrap(),
            ModuleIdentity::parse("//libs/Common").unwrap()
        );
        assert_eq!(
            ModuleIdentity::parse("//libs/Common").unwrap().to_build_file().as_ref(),
            Path::new("libs/Common/sindri.build")
        );
        let mut identities: HashSet<ModuleIdentity> = HashSet::new();
        assert!(identities.insert(ModuleIdentity::parse("//libs/common").unwrap()));
        assert!(identities.insert(ModuleIdentity::parse("//libs/Common").unwrap()));
    }

    #[test]
    fn module_identity_qualifier_is_case_insensitive() {
        let canonical: ModuleIdentity = ModuleIdentity::parse("//libs/common [kotlin]").unwrap();
        assert_eq!(canonical.to_string(), "//libs/common/ [kotlin]");
        for text in [
            "//libs/common [kotlin]",
            "//libs/common [Kotlin]",
            "//libs/common [KOTLIN]",
        ] {
            let identity: ModuleIdentity = ModuleIdentity::parse(text).unwrap();
            assert_eq!(identity, canonical);
            assert_eq!(
                identity.to_build_file().as_ref(),
                Path::new("libs/common/sindri-kotlin.build")
            );
        }
    }

    #[test]
    fn module_identity_allows_aligned_qualifiers_with_multiple_spaces() {
        assert_eq!(
            ModuleIdentity::parse("//libs/common     [kotlin]").unwrap(),
            ModuleIdentity::parse("//libs/common [kotlin]").unwrap()
        );
    }

    #[test]
    fn module_identity_requires_at_least_one_space_before_the_qualifier() {
        assert!(ModuleIdentity::parse("//libs/common[kotlin]").is_err());
    }

    #[test]
    fn module_identity_requires_the_double_slash_prefix() {
        assert!(ModuleIdentity::parse("libs/common").is_err());
    }

    #[test]
    fn module_identity_rejects_spaces_within_a_directory_name() {
        assert!(ModuleIdentity::parse("//libs/my common").is_err());
        assert!(ModuleIdentity::parse("//libs/my common [kotlin]").is_err());
    }

    #[test]
    fn module_identity_rejects_brackets_within_a_directory_name() {
        // The former pathological case: a directory literally named `my-dir[kotlin]` is a clean
        // rejection now, not a silent mis-resolution to `my-dir/sindri-kotlin.build`.
        assert!(ModuleIdentity::parse("//my-dir[kotlin]").is_err());
    }

    #[test]
    fn module_identity_rejects_traversal_segments() {
        assert!(ModuleIdentity::parse("//libs/../secret").is_err());
        assert!(ModuleIdentity::parse("//.").is_err());
    }

    #[test]
    fn module_identity_round_trips_through_display() {
        // Display's canonical form always ends the directory in `/` (RelativeDirectory::Named's own
        // invariant); parse accepts that form back, so the two round-trip through each other.
        for text in ["//libs/common/", "//tools/codegen/ [bin]"] {
            assert_eq!(ModuleIdentity::parse(text).unwrap().to_string(), text);
        }
    }

    #[test]
    fn module_identity_parse_tolerates_a_missing_trailing_slash() {
        // The bare form users already write in `dependencies` declarations still parses, and is the
        // same identity as the canonical, slash-terminated form Display produces.
        assert_eq!(
            ModuleIdentity::parse("//libs/common").unwrap(),
            ModuleIdentity::parse("//libs/common/").unwrap()
        );
    }

    #[test]
    fn module_identity_qualifier_allows_punctuation_and_single_letters() {
        for text in [
            "//libs/common [c]",
            "//libs/common [c++]",
            "//libs/common [f#]",
            "//libs/tools [web.archive]",
        ] {
            assert!(ModuleIdentity::parse(text).is_ok(), "expected `{text}` to parse");
        }
    }

    #[test]
    fn module_identity_qualifier_rejects_bad_starts_and_trailing_dots() {
        // Must start with a letter (so never `.`/`..` or a leading digit) and must not end in `.` — an
        // illegal directory name on Windows, where the qualifier becomes a build-state directory.
        for text in [
            "//libs/common [1x]",
            "//libs/common [_x]",
            "//libs/common [+x]",
            "//libs/common [web.]",
        ] {
            assert!(ModuleIdentity::parse(text).is_err(), "expected `{text}` to be rejected");
        }
    }

    #[test]
    fn working_directory_relativizes_against_the_root_and_resolves_back() {
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let absolute: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/services/api"));
        let derived: WorkingDirectory = WorkingDirectory::derive(&absolute, &root);
        assert_eq!(derived.as_ref(), Path::new("services/api/"));
        assert_eq!(derived.absolute(&root), absolute);
        let constructed: WorkingDirectory = WorkingDirectory::new(RelativeDirectory::new_unchecked("services/api"));
        assert_eq!(constructed.absolute(&root), absolute);
    }

    #[test]
    fn language_deserialize_rejects_an_unknown_language() {
        assert!(serde_json::from_str::<Language>(r#""rust""#).is_err());
    }

    #[test]
    fn relative_file_new_accepts_a_well_formed_path() {
        assert_eq!(
            RelativeFile::new("libs/common/main.go").unwrap().as_ref(),
            Path::new("libs/common/main.go")
        );
    }

    #[test]
    fn relative_file_new_rejects_an_absolute_path() {
        let error: InvalidRelativePath = RelativeFile::new("/etc").unwrap_err();
        assert!(error.to_string().contains("absolute path"), "message was: {error}");
    }

    #[test]
    fn relative_file_new_rejects_a_repeated_slash() {
        let error: InvalidRelativePath = RelativeFile::new("libs//common").unwrap_err();
        assert!(error.to_string().contains("no repeated"), "message was: {error}");
    }

    #[test]
    fn relative_file_new_rejects_a_trailing_slash() {
        let error: InvalidRelativePath = RelativeFile::new("libs/common/").unwrap_err();
        assert!(error.to_string().contains("directory-like"), "message was: {error}");
    }

    #[test]
    fn relative_directory_new_accepts_the_empty_directory() {
        assert_eq!(RelativeDirectory::new("").unwrap(), RelativeDirectory::Dot);
        assert_eq!(RelativeDirectory::new("").unwrap().as_ref(), Path::new(""));
    }

    #[test]
    fn relative_directory_new_accepts_an_already_slash_terminated_path() {
        // Already exactly one trailing slash: `new` reuses the value as-is rather than
        // reformatting it — confirmed by the result still being exactly one trailing slash, not two.
        assert_eq!(
            RelativeDirectory::new("libs/common/").unwrap().as_ref(),
            Path::new("libs/common/")
        );
    }

    #[test]
    fn relative_directory_new_rejects_an_absolute_path() {
        let error: InvalidRelativePath = RelativeDirectory::new("/etc").unwrap_err();
        assert!(error.to_string().contains("absolute path"), "message was: {error}");
    }

    #[test]
    fn relative_directory_new_rejects_a_repeated_slash() {
        let error: InvalidRelativePath = RelativeDirectory::new("libs//common/").unwrap_err();
        assert!(error.to_string().contains("no repeated"), "message was: {error}");
    }

    #[test]
    fn relative_directory_new_rejects_a_missing_trailing_slash() {
        let error: InvalidRelativePath = RelativeDirectory::new("libs/common").unwrap_err();
        assert!(error.to_string().contains("ending in"), "message was: {error}");
    }

    #[test]
    fn relative_directory_new_unchecked_normalizes_a_missing_or_doubled_trailing_slash() {
        assert_eq!(
            RelativeDirectory::new_unchecked("libs/common").as_ref(),
            Path::new("libs/common/")
        );
        assert_eq!(
            RelativeDirectory::new_unchecked("libs/common//").as_ref(),
            Path::new("libs/common/")
        );
    }

    #[test]
    fn absolute_directory_new_normalizes_a_missing_trailing_slash_and_reuses_an_existing_one() {
        assert_eq!(
            AbsoluteDirectory::new(PathBuf::from("/workspace")).as_ref(),
            Path::new("/workspace/")
        );
        assert_eq!(
            AbsoluteDirectory::new(PathBuf::from("/workspace/")).as_ref(),
            Path::new("/workspace/")
        );
        assert_eq!(AbsoluteDirectory::new(PathBuf::from("/")).as_ref(), Path::new("/"));
    }

    #[test]
    fn module_identity_parse_error_reports_the_offending_text() {
        let error: ModuleIdentityParseError = ModuleIdentity::parse("not-an-identity").unwrap_err();
        let message: String = error.to_string();
        assert!(message.contains("not-an-identity"), "message was: {message}");
        assert!(message.contains("invalid module identity"), "message was: {message}");
    }

    #[test]
    fn command_output_exposes_its_streams_and_status() {
        let output: CommandOutput = CommandOutput::new(
            Stdout::new(b"out".to_vec()),
            Stderr::new(b"err".to_vec()),
            TaskStatus::Succeeded,
        );
        assert_eq!(output.stdout().as_bytes(), b"out");
        assert_eq!(output.stderr().as_bytes(), b"err");
        assert_eq!(output.status(), TaskStatus::Succeeded);
    }

    #[test]
    fn stdout_captures_writes_and_flush_is_a_noop() {
        let mut stdout: Stdout = Stdout::new(Vec::new());
        stdout.write_all(b"hello").unwrap();
        stdout.flush().unwrap();
        assert_eq!(stdout.as_bytes(), b"hello");
    }

    #[test]
    fn workspace_root_and_build_file_display_and_resolve() {
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        assert_eq!(root.to_string(), "/workspace/");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        assert_eq!(build_file.to_string(), "sindri.build");
        assert_eq!(
            build_file.absolute(&root).as_ref(),
            Path::new("/workspace/sindri.build")
        );
    }
}
