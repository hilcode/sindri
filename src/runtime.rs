use crate::file_set::CompiledFileSetPattern;
use crate::file_set::FileSetPattern;
use crate::local_time_with_elapsed::LocalTimeWithElapsed;
use crate::script::Command;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::BuildStart;
use crate::types::CommandOutput;
use crate::types::DirEntry;
use crate::types::FileKind;
use crate::types::FileMetadata;
use crate::types::WorkspaceRoot;
use ignore::DirEntry as WalkEntry;
use ignore::WalkBuilder;
use std::env::current_dir;
use std::fs::DirEntry as DirectoryEntry;
use std::fs::File;
use std::fs::Metadata;
use std::fs::OpenOptions;
use std::fs::create_dir_all;
use std::fs::metadata;
use std::fs::read;
use std::fs::read_dir;
use std::fs::read_to_string;
use std::fs::symlink_metadata;
use std::fs::write;
use std::io::Error as IoError;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::io::Write;
use std::io::stdout;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::Output;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Instant;
use time::UtcOffset;

#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use std::time::SystemTime;

/// Read/write access to the filesystem. This is the only capability the bootstrap phase needs:
/// the workspace is located and its configuration read before any [`Runtime`] exists, because the
/// log destination — and thus the runtime — is not known until then.
pub trait FileSystem {
    fn current_directory(&self) -> IoResult<PathBuf>;
    fn read_to_string(&self, path: &Path) -> IoResult<String>;
    /// The raw bytes of a file, with no line-ending or encoding normalization — that belongs to the
    /// hashing layer, which needs the bytes exactly as stored.
    fn read(&self, path: &Path) -> IoResult<Vec<u8>>;
    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>>;
    /// Every regular file beneath `base` selected by the ordered include globs of `pattern`, as
    /// absolute paths. Drives a recursive walk that classifies each file it finds.
    fn matching_file_set(&self, base: &Path, pattern: &FileSetPattern) -> IoResult<Vec<PathBuf>>;
    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>>;
    fn file_metadata(&self, path: &Path) -> IoResult<FileMetadata>;
    fn write(&self, path: &Path, contents: &[u8]) -> IoResult<()>;
    fn create_directories(&self, path: &Path) -> IoResult<()>;
}

/// Every side effect a build performs: the [`FileSystem`], plus a clock, command execution,
/// diagnostic logging, and user-facing output. Shared across the executor's worker threads, so
/// every method takes `&self`. Built once the log destination is known — see
/// [`Bootstrap::into_runtime`].
pub trait Runtime: FileSystem + Sync {
    fn now(&self) -> Instant;
    fn run_command(&self, command: &Command, workspace_root: &WorkspaceRoot) -> IoResult<CommandOutput>;
    fn log(&self, message: &str) -> IoResult<()>;
    fn output(&self) -> impl Write + '_;
}

/// A [`FileSystem`] that can be promoted into a full [`Runtime`]. The workspace must be located
/// first — its build directory is where the log lives — so this consumes the bootstrap filesystem
/// and hands back a runtime that owns the already-opened log sink, with no later mutation.
pub trait Bootstrap: FileSystem {
    fn into_runtime(self, start: BuildStart, log_path: Option<AbsoluteFile>) -> IoResult<impl Runtime>;
}

pub struct SystemFileSystem;

impl FileSystem for SystemFileSystem {
    fn current_directory(&self) -> IoResult<PathBuf> {
        current_dir()
    }

    fn read_to_string(&self, path: &Path) -> IoResult<String> {
        read_to_string(path)
    }

    fn read(&self, path: &Path) -> IoResult<Vec<u8>> {
        read(path)
    }

    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>> {
        let mut entries: Vec<DirEntry> = Vec::new();
        for entry in read_dir(path)? {
            let entry: DirectoryEntry = entry?;
            let kind: FileKind = entry.file_type()?.into();
            entries.push(DirEntry::new(entry.path(), kind));
        }
        Ok(entries)
    }

    fn matching_file_set(&self, base: &Path, pattern: &FileSetPattern) -> IoResult<Vec<PathBuf>> {
        // A task's output directory does not exist yet before its first successful run, and a
        // resolved FileSet is queried against it regardless (to judge dirtiness before running, and
        // to hash a fresh output afterwards) — a missing base is an empty set, not a walk error.
        if self.file_kind(base)?.is_none() {
            return Ok(Vec::new());
        }
        let compiled: CompiledFileSetPattern = pattern.compile().map_err(IoError::other)?;
        let mut walker: WalkBuilder = WalkBuilder::new(base);
        walker.standard_filters(false).follow_links(false);
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in walker.build() {
            let entry: WalkEntry = entry.map_err(IoError::other)?;
            if entry.file_type().is_some_and(|file_type| file_type.is_file()) {
                let relative: &Path = entry.path().strip_prefix(base).unwrap_or_else(|_| entry.path());
                if compiled.classify_file(relative) {
                    files.push(entry.into_path());
                }
            }
        }
        Ok(files)
    }

    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>> {
        match symlink_metadata(path) {
            Ok(metadata) => Ok(Some(metadata.file_type().into())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn file_metadata(&self, path: &Path) -> IoResult<FileMetadata> {
        let file_metadata: Metadata = metadata(path)?;
        Ok(FileMetadata::new(file_metadata.len(), file_metadata.modified()?))
    }

    fn write(&self, path: &Path, contents: &[u8]) -> IoResult<()> {
        write(path, contents)
    }

    fn create_directories(&self, path: &Path) -> IoResult<()> {
        create_dir_all(path)
    }
}

impl Bootstrap for SystemFileSystem {
    fn into_runtime(self, start: BuildStart, log_path: Option<AbsoluteFile>) -> IoResult<impl Runtime> {
        // Reading the system timezone is fallible and, under the `time` crate, refuses to run
        // once other threads exist. We snapshot it here, during bootstrap, before any thread is
        // spawned, so the read succeeds; UTC is a defensive last resort, not a routine fallback.
        // The captured offset then renders every log line without touching the system timezone.
        let offset: UtcOffset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
        // Opening the log sink is itself a filesystem operation, so it happens here, on the
        // bootstrap filesystem; the runtime then owns the file from the moment it is built.
        let log_file: Option<File> = match log_path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    self.create_directories(parent.as_ref())?;
                }
                Some(OpenOptions::new().create(true).append(true).open(path.as_ref())?)
            }
            None => None,
        };
        Ok(SystemRuntime {
            file_system: self,
            time: LocalTimeWithElapsed::new(start, offset),
            log_file: Mutex::new(log_file),
        })
    }
}

pub struct SystemRuntime {
    file_system: SystemFileSystem,
    time: LocalTimeWithElapsed,
    log_file: Mutex<Option<File>>,
}

impl FileSystem for SystemRuntime {
    fn current_directory(&self) -> IoResult<PathBuf> {
        self.file_system.current_directory()
    }

    fn read_to_string(&self, path: &Path) -> IoResult<String> {
        self.file_system.read_to_string(path)
    }

    fn read(&self, path: &Path) -> IoResult<Vec<u8>> {
        self.file_system.read(path)
    }

    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>> {
        self.file_system.read_directory(path)
    }

    fn matching_file_set(&self, base: &Path, pattern: &FileSetPattern) -> IoResult<Vec<PathBuf>> {
        self.file_system.matching_file_set(base, pattern)
    }

    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>> {
        self.file_system.file_kind(path)
    }

    fn file_metadata(&self, path: &Path) -> IoResult<FileMetadata> {
        self.file_system.file_metadata(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> IoResult<()> {
        self.file_system.write(path, contents)
    }

    fn create_directories(&self, path: &Path) -> IoResult<()> {
        self.file_system.create_directories(path)
    }
}

impl Runtime for SystemRuntime {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn run_command(&self, command: &Command, workspace_root: &WorkspaceRoot) -> IoResult<CommandOutput> {
        let working_directory: AbsoluteDirectory = workspace_root
            .to_absolute_directory()
            .join_directory(command.working_directory());
        let output: Output = ProcessCommand::new(command.program())
            .args(command.arguments().iter().map(|argument| argument.as_str()))
            .envs(
                command
                    .environment()
                    .iter()
                    .map(|(name, value)| (name.as_str(), value.as_str())),
            )
            .current_dir(working_directory.as_ref())
            .output()?;
        Ok(CommandOutput::new(
            output.stdout.into(),
            output.stderr.into(),
            output.status.into(),
        ))
    }

    fn log(&self, message: &str) -> IoResult<()> {
        // Logging is opt-in (`--log`); when disabled there is no file and nothing to write.
        // When enabled, a write failure means the disk is full or failing, so we surface it
        // rather than hide the earliest symptom of a problem the build is about to hit too.
        let mut log_file: MutexGuard<'_, Option<File>> = self.log_file.lock().unwrap();
        match log_file.as_mut() {
            Some(file) => writeln!(file, "{} {message}", self.time.format()),
            None => Ok(()),
        }
    }

    fn output(&self) -> impl Write + '_ {
        stdout()
    }
}

#[cfg(test)]
use crate::types::Stdout;
#[cfg(test)]
use smol_str::SmolStr;
#[cfg(test)]
use std::cmp::Ordering;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;

/// A [`Write`] handle over a [`DummyRuntime`]'s captured standard output. Holds the lock for the
/// duration of the write, so each `writeln!` lands atomically.
#[cfg(test)]
struct CapturedOutput<'a>(MutexGuard<'a, Stdout>);

#[cfg(test)]
impl Write for CapturedOutput<'_> {
    fn write(&mut self, buffer: &[u8]) -> IoResult<usize> {
        self.0.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

/// The key a [`Runtime::run_command`] stub is registered and looked up under: a literal string
/// checked against the rendered command line (`program` followed by its space-joined arguments, e.g.
/// `"go build -o /tmp/xyz/output/ ./..."`) with [`str::starts_with`] — not a glob or any other pattern
/// syntax. A test typically registers only as much of the line as it can predict (`"go build"`,
/// leaving off a resolved `{output}` path it cannot spell out in advance), which is exactly why this
/// is a prefix rather than requiring the full line. Choosing one that is also a prefix of some other
/// registered stub's command line is a test bug — e.g. `"go"` would ambiguously match both `go build`
/// and `go test` — but nothing here checks for that; it is on the test author to keep prefixes
/// unambiguous within one [`DummyRuntime`].
#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CommandPrefix(SmolStr);

#[cfg(test)]
impl CommandPrefix {
    fn matches(&self, command_line: &str) -> bool {
        command_line.starts_with(self.0.as_str())
    }
}

#[cfg(test)]
impl From<&str> for CommandPrefix {
    fn from(prefix: &str) -> CommandPrefix {
        CommandPrefix(SmolStr::new(prefix))
    }
}

/// A registered [`Runtime::run_command`] stub: either a canned [`CommandOutput`] returned as-is, or a
/// handler run on the calling thread each time the stub is matched — the calling thread being one of
/// the real OS threads the executor spawns per concurrent task, so a handler can coordinate with
/// another handler running at the same time (e.g. via a shared [`std::sync::Barrier`]) to prove
/// genuine concurrent dispatch deterministically, without timing anything.
#[cfg(test)]
enum CommandStub {
    Output(CommandOutput),
    Handler(Box<dyn Fn() -> CommandOutput + Send + Sync>),
}

/// A registered file's content and modification time, standing in for what a real filesystem's
/// `stat`/read calls would report.
#[cfg(test)]
struct StoredFile {
    contents: Vec<u8>,
    modified: SystemTime,
}

/// An in-memory [`Runtime`] for hermetic tests. Build one with [`DummyRuntime::builder`],
/// registering the files, directories, symlinks and canned command outputs a test needs. It also
/// implements [`FileSystem`] and [`Bootstrap`], so it can stand in anywhere from bootstrap onward.
#[cfg(test)]
pub struct DummyRuntime {
    files: HashMap<PathBuf, StoredFile>,
    directories: HashSet<PathBuf>,
    symlinks: HashSet<PathBuf>,
    commands: HashMap<CommandPrefix, CommandStub>,
    current_directory: PathBuf,
    current_directory_error: Option<ErrorKind>,
    errors: HashMap<PathBuf, ErrorKind>,
    now: Instant,
    writes: Mutex<HashMap<PathBuf, Vec<u8>>>,
    created_directories: Mutex<HashSet<PathBuf>>,
    logs: Mutex<Vec<String>>,
    output: Mutex<Stdout>,
    reads: Mutex<Vec<PathBuf>>,
}

#[cfg(test)]
impl DummyRuntime {
    pub fn builder() -> DummyRuntimeBuilder {
        DummyRuntimeBuilder::new()
    }

    /// The bytes written to `path` by [`FileSystem::write`], if any.
    pub fn written_file(&self, path: impl AsRef<Path>) -> Option<Vec<u8>> {
        self.writes.lock().unwrap().get(path.as_ref()).cloned()
    }

    /// Every path written by [`FileSystem::write`], paired with its bytes. Lets a test replay a second
    /// build with the first build's persisted state visible as real files: [`FileSystem::write`] records
    /// into a log that [`FileSystem::read`] does not consult, so without re-registering these a replayed
    /// build would treat every task as uncached.
    pub fn written_files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .map(|(path, contents): (&PathBuf, &Vec<u8>)| -> (PathBuf, Vec<u8>) { (path.clone(), contents.clone()) })
            .collect()
    }

    /// How many times [`FileSystem::read`] was called for `path`. Lets a test prove a caching layer
    /// actually avoids redundant reads, rather than merely returning the right answer.
    pub fn read_count(&self, path: impl AsRef<Path>) -> usize {
        self.reads
            .lock()
            .unwrap()
            .iter()
            .filter(|read: &&PathBuf| -> bool { read.as_path() == path.as_ref() })
            .count()
    }

    /// Whether [`FileSystem::create_directories`] was called for `path`.
    pub fn created_directory(&self, path: impl AsRef<Path>) -> bool {
        self.created_directories.lock().unwrap().contains(path.as_ref())
    }

    /// The messages passed to [`Runtime::log`], in the order they were logged.
    pub fn logged(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }

    /// Everything written to [`Runtime::output`] so far.
    pub fn captured_output(&self) -> Stdout {
        self.output.lock().unwrap().clone()
    }

    fn kind_of(&self, path: &Path) -> Option<FileKind> {
        if self.symlinks.contains(path) {
            Some(FileKind::Symlink)
        } else if self.files.contains_key(path) {
            Some(FileKind::File)
        } else if self.directories.contains(path) {
            Some(FileKind::Directory)
        } else {
            None
        }
    }
}

#[cfg(test)]
impl FileSystem for DummyRuntime {
    fn current_directory(&self) -> IoResult<PathBuf> {
        match self.current_directory_error {
            Some(kind) => Err(IoError::new(kind, "simulated current-directory failure")),
            None => Ok(self.current_directory.clone()),
        }
    }

    fn read_to_string(&self, path: &Path) -> IoResult<String> {
        match self.files.get(path) {
            Some(file) => {
                String::from_utf8(file.contents.clone()).map_err(|error| IoError::new(ErrorKind::InvalidData, error))
            }
            None => Err(IoError::new(
                ErrorKind::NotFound,
                format!("no such file: {}", path.display()),
            )),
        }
    }

    fn read(&self, path: &Path) -> IoResult<Vec<u8>> {
        self.reads.lock().unwrap().push(path.to_path_buf());
        match self.files.get(path) {
            Some(file) => Ok(file.contents.clone()),
            None => Err(IoError::new(
                ErrorKind::NotFound,
                format!("no such file: {}", path.display()),
            )),
        }
    }

    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>> {
        if let Some(kind) = self.errors.get(path) {
            return Err(IoError::new(
                *kind,
                format!("simulated failure reading {}", path.display()),
            ));
        }
        if self.kind_of(path) != Some(FileKind::Directory) {
            return Err(IoError::new(
                ErrorKind::NotFound,
                format!("no such directory: {}", path.display()),
            ));
        }
        let mut children: HashSet<PathBuf> = HashSet::new();
        let known_paths = self
            .files
            .keys()
            .chain(self.directories.iter())
            .chain(self.symlinks.iter());
        for candidate in known_paths {
            if candidate.parent() == Some(path) {
                children.insert(candidate.clone());
            }
        }
        let mut entries: Vec<DirEntry> = children
            .into_iter()
            .map(|child: PathBuf| -> DirEntry {
                let kind: FileKind = self.kind_of(&child).expect("a known path always has a kind");
                DirEntry::new(child, kind)
            })
            .collect();
        entries.sort_by(|first: &DirEntry, second: &DirEntry| -> Ordering { first.path().cmp(second.path()) });
        Ok(entries)
    }

    fn matching_file_set(&self, base: &Path, pattern: &FileSetPattern) -> IoResult<Vec<PathBuf>> {
        // The dummy holds its files in a flat in-memory map with no on-disk directory tree, so there
        // is nothing to walk: iterate every registered file under `base` and classify it directly.
        let compiled: CompiledFileSetPattern = pattern.compile().map_err(IoError::other)?;
        let mut files: Vec<PathBuf> = Vec::new();
        for path in self.files.keys() {
            if let Ok(relative) = path.strip_prefix(base) {
                if compiled.classify_file(relative) {
                    files.push(path.clone());
                }
            }
        }
        Ok(files)
    }

    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>> {
        match self.errors.get(path) {
            Some(kind) => Err(IoError::new(
                *kind,
                format!("simulated failure reading {}", path.display()),
            )),
            None => Ok(self.kind_of(path)),
        }
    }

    fn file_metadata(&self, path: &Path) -> IoResult<FileMetadata> {
        match self.files.get(path) {
            Some(file) => Ok(FileMetadata::new(file.contents.len() as u64, file.modified)),
            None => Err(IoError::new(
                ErrorKind::NotFound,
                format!("no such file: {}", path.display()),
            )),
        }
    }

    fn write(&self, path: &Path, contents: &[u8]) -> IoResult<()> {
        self.writes
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), contents.to_vec());
        Ok(())
    }

    fn create_directories(&self, path: &Path) -> IoResult<()> {
        self.created_directories.lock().unwrap().insert(path.to_path_buf());
        Ok(())
    }
}

#[cfg(test)]
impl Runtime for DummyRuntime {
    fn now(&self) -> Instant {
        self.now
    }

    fn run_command(&self, command: &Command, _workspace_root: &WorkspaceRoot) -> IoResult<CommandOutput> {
        let command_line: String = command.to_string();
        match self
            .commands
            .iter()
            .find(|(prefix, _)| -> bool { prefix.matches(&command_line) })
        {
            Some((_, CommandStub::Output(output))) => Ok(output.clone()),
            Some((_, CommandStub::Handler(handler))) => Ok(handler()),
            None => Err(IoError::new(
                ErrorKind::NotFound,
                format!("command not stubbed: {command}"),
            )),
        }
    }

    fn log(&self, message: &str) -> IoResult<()> {
        self.logs.lock().unwrap().push(message.to_string());
        Ok(())
    }

    fn output(&self) -> impl Write + '_ {
        CapturedOutput(self.output.lock().unwrap())
    }
}

#[cfg(test)]
impl Bootstrap for DummyRuntime {
    fn into_runtime(self, _start: BuildStart, _log_path: Option<AbsoluteFile>) -> IoResult<impl Runtime> {
        // A dummy is already a fully-formed runtime; the start instant and log path are real-system
        // concerns it does not need, so it simply hands itself back.
        Ok(self)
    }
}

#[cfg(test)]
pub struct DummyRuntimeBuilder {
    files: HashMap<PathBuf, StoredFile>,
    directories: HashSet<PathBuf>,
    symlinks: HashSet<PathBuf>,
    commands: HashMap<CommandPrefix, CommandStub>,
    current_directory: PathBuf,
    current_directory_error: Option<ErrorKind>,
    errors: HashMap<PathBuf, ErrorKind>,
    now: Instant,
    /// The modification time the next plain [`file`](DummyRuntimeBuilder::file) call assigns,
    /// advanced by one second each time — so files registered in separate calls get distinct,
    /// deterministic timestamps without a test having to name one explicitly.
    next_modified: SystemTime,
}

#[cfg(test)]
impl DummyRuntimeBuilder {
    pub fn new() -> DummyRuntimeBuilder {
        DummyRuntimeBuilder {
            files: HashMap::new(),
            directories: HashSet::new(),
            symlinks: HashSet::new(),
            commands: HashMap::new(),
            current_directory: PathBuf::from("/"),
            current_directory_error: None,
            errors: HashMap::new(),
            now: Instant::now(),
            next_modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1),
        }
    }

    /// Register a file with the given contents, modified at an unspecified but deterministic time
    /// distinct from every other file registered this way. Ancestor directories are created
    /// automatically.
    pub fn file(mut self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> DummyRuntimeBuilder {
        let modified: SystemTime = self.next_modified;
        self.next_modified += Duration::from_secs(1);
        self.file_modified_at(path, contents, modified)
    }

    /// Register a file with the given contents and an explicit modification time — for a test that
    /// needs precise control over whether two files' (or two versions of one file's) timestamps are
    /// equal or distinct, rather than the arbitrary but deterministic time [`file`](Self::file) picks.
    pub fn file_modified_at(
        mut self,
        path: impl AsRef<Path>,
        contents: impl AsRef<[u8]>,
        modified: SystemTime,
    ) -> DummyRuntimeBuilder {
        let path: PathBuf = path.as_ref().to_path_buf();
        self.register_ancestors(&path);
        self.files.insert(
            path,
            StoredFile {
                contents: contents.as_ref().to_vec(),
                modified,
            },
        );
        self
    }

    /// Register a directory. Ancestor directories are created automatically.
    pub fn directory(mut self, path: impl AsRef<Path>) -> DummyRuntimeBuilder {
        let path: PathBuf = path.as_ref().to_path_buf();
        self.register_ancestors(&path);
        self.directories.insert(path);
        self
    }

    /// Register a symlink. Ancestor directories are created automatically.
    pub fn symlink(mut self, path: impl AsRef<Path>) -> DummyRuntimeBuilder {
        let path: PathBuf = path.as_ref().to_path_buf();
        self.register_ancestors(&path);
        self.symlinks.insert(path);
        self
    }

    /// Register the output [`Runtime::run_command`] should return for `command`.
    pub fn command(mut self, command: impl Into<CommandPrefix>, output: CommandOutput) -> DummyRuntimeBuilder {
        self.commands.insert(command.into(), CommandStub::Output(output));
        self
    }

    /// Register a handler run on the calling thread each time `command` is matched, instead of a
    /// fixed output — e.g. to synchronize with another concurrently running stub before returning.
    pub fn command_handler(
        mut self,
        command: impl Into<CommandPrefix>,
        handler: impl Fn() -> CommandOutput + Send + Sync + 'static,
    ) -> DummyRuntimeBuilder {
        self.commands
            .insert(command.into(), CommandStub::Handler(Box::new(handler)));
        self
    }

    pub fn current_directory(mut self, path: impl AsRef<Path>) -> DummyRuntimeBuilder {
        self.current_directory = path.as_ref().to_path_buf();
        self
    }

    /// Make [`FileSystem::current_directory`] fail with `kind` instead of returning the configured
    /// current directory.
    pub fn current_directory_error(mut self, kind: ErrorKind) -> DummyRuntimeBuilder {
        self.current_directory_error = Some(kind);
        self
    }

    /// Make [`FileSystem::file_kind`] and [`FileSystem::read_directory`] fail with `kind` when called
    /// on `path`, instead of consulting the registered files/directories/symlinks.
    pub fn error(mut self, path: impl AsRef<Path>, kind: ErrorKind) -> DummyRuntimeBuilder {
        self.errors.insert(path.as_ref().to_path_buf(), kind);
        self
    }

    pub fn build(self) -> DummyRuntime {
        DummyRuntime {
            files: self.files,
            directories: self.directories,
            symlinks: self.symlinks,
            commands: self.commands,
            current_directory: self.current_directory,
            current_directory_error: self.current_directory_error,
            errors: self.errors,
            now: self.now,
            writes: Mutex::new(HashMap::new()),
            created_directories: Mutex::new(HashSet::new()),
            logs: Mutex::new(Vec::new()),
            output: Mutex::new(Stdout::default()),
            reads: Mutex::new(Vec::new()),
        }
    }

    fn register_ancestors(&mut self, path: &Path) {
        let mut ancestor: Option<&Path> = path.parent();
        while let Some(directory) = ancestor {
            if directory.as_os_str().is_empty() {
                break;
            }
            self.directories.insert(directory.to_path_buf());
            ancestor = directory.parent();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Stderr;
    use crate::types::TaskStatus;

    #[test]
    fn file_kind_reports_files_directories_symlinks_and_absence() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.workspace", "name = \"demo\"")
            .directory("/workspace/source")
            .symlink("/workspace/link")
            .build();
        assert_eq!(
            runtime.file_kind(Path::new("/workspace/sindri.workspace")).unwrap(),
            Some(FileKind::File)
        );
        assert_eq!(
            runtime.file_kind(Path::new("/workspace/source")).unwrap(),
            Some(FileKind::Directory)
        );
        assert_eq!(
            runtime.file_kind(Path::new("/workspace/link")).unwrap(),
            Some(FileKind::Symlink)
        );
        // Ancestor directories are registered automatically.
        assert_eq!(
            runtime.file_kind(Path::new("/workspace")).unwrap(),
            Some(FileKind::Directory)
        );
        assert_eq!(runtime.file_kind(Path::new("/workspace/missing")).unwrap(), None);
    }

    #[test]
    fn read_to_string_returns_registered_contents() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/file.ncl", "contents").build();
        assert_eq!(runtime.read_to_string(Path::new("/file.ncl")).unwrap(), "contents");
        assert_eq!(
            runtime.read_to_string(Path::new("/absent")).unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn read_directory_lists_immediate_children_with_their_kinds() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/sindri.build", "")
            .directory("/workspace/source")
            .symlink("/workspace/link")
            .file("/workspace/source/deep.txt", "")
            .build();
        let entries: Vec<DirEntry> = runtime.read_directory(Path::new("/workspace")).unwrap();
        let listing: Vec<(String, FileKind)> = entries
            .iter()
            .map(|entry: &DirEntry| -> (String, FileKind) { (entry.file_name().into_owned(), entry.kind()) })
            .collect();
        assert_eq!(
            listing,
            vec![
                ("link".to_string(), FileKind::Symlink),
                ("sindri.build".to_string(), FileKind::File),
                ("source".to_string(), FileKind::Directory),
            ]
        );
    }

    #[test]
    fn read_directory_rejects_non_directories() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/file", "").build();
        assert_eq!(
            runtime.read_directory(Path::new("/file")).unwrap_err().kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn run_command_returns_stubbed_output_and_errors_otherwise() {
        let stubbed: CommandOutput =
            CommandOutput::new(Stdout::new(b"ok".to_vec()), Stderr::default(), TaskStatus::Succeeded);
        let runtime: DummyRuntime = DummyRuntime::builder().command("go build", stubbed).build();
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let output: CommandOutput = runtime
            .run_command(&Command::new("go", ["build"]), &workspace_root)
            .unwrap();
        assert_eq!(output.stdout().as_bytes(), b"ok");
        assert_eq!(output.status(), TaskStatus::Succeeded);
        assert_eq!(
            runtime
                .run_command(&Command::new("go", ["test"]), &workspace_root)
                .unwrap_err()
                .kind(),
            ErrorKind::NotFound
        );
    }

    #[test]
    fn write_and_create_directories_are_recorded() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        runtime.create_directories(Path::new("/output")).unwrap();
        runtime.write(Path::new("/output/telemetry.json"), b"{}").unwrap();
        assert!(runtime.created_directory("/output"));
        assert_eq!(runtime.written_file("/output/telemetry.json"), Some(b"{}".to_vec()));
        assert_eq!(runtime.written_file("/output/absent"), None);
    }

    #[test]
    fn current_directory_returns_the_configured_path() {
        let runtime: DummyRuntime = DummyRuntime::builder().current_directory("/workspace/source").build();
        assert_eq!(runtime.current_directory().unwrap(), PathBuf::from("/workspace/source"));
    }

    #[test]
    fn current_directory_error_overrides_the_configured_path() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .current_directory_error(ErrorKind::PermissionDenied)
            .build();
        assert_eq!(
            runtime.current_directory().unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn error_makes_file_kind_and_read_directory_fail_for_that_path() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .directory("/workspace")
            .error("/workspace/sindri.build", ErrorKind::PermissionDenied)
            .error("/workspace", ErrorKind::PermissionDenied)
            .build();
        assert_eq!(
            runtime
                .file_kind(Path::new("/workspace/sindri.build"))
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
        assert_eq!(
            runtime.read_directory(Path::new("/workspace")).unwrap_err().kind(),
            ErrorKind::PermissionDenied
        );
    }
}
