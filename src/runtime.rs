use crate::local_time_with_elapsed::LocalTimeWithElapsed;
use crate::types::AbsoluteFile;
use crate::types::BuildStart;
use crate::types::CommandOutput;
use crate::types::DirEntry;
use crate::types::FileKind;
use crate::types::ShellCommand;
use std::env::current_dir;
use std::fs::DirEntry as DirectoryEntry;
use std::fs::File;
use std::fs::OpenOptions;
use std::fs::create_dir_all;
use std::fs::read_dir;
use std::fs::read_to_string;
use std::fs::symlink_metadata;
use std::fs::write;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::io::Write;
use std::io::stdout;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use std::str::SplitAsciiWhitespace;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::time::Instant;
use time::UtcOffset;

/// Read/write access to the filesystem. This is the only capability the bootstrap phase needs:
/// the workspace is located and its configuration read before any [`Runtime`] exists, because the
/// log destination — and thus the runtime — is not known until then.
pub trait FileSystem {
    fn current_directory(&self) -> IoResult<PathBuf>;
    fn read_to_string(&self, path: &Path) -> IoResult<String>;
    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>>;
    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>>;
    fn write(&self, path: &Path, contents: &[u8]) -> IoResult<()>;
    fn create_directories(&self, path: &Path) -> IoResult<()>;
}

/// Every side effect a build performs: the [`FileSystem`], plus a clock, command execution,
/// diagnostic logging, and user-facing output. Shared across the executor's worker threads, so
/// every method takes `&self`. Built once the log destination is known — see
/// [`Bootstrap::into_runtime`].
pub trait Runtime: FileSystem + Sync {
    fn now(&self) -> Instant;
    fn run_command(&self, command: &ShellCommand, working_directory: &Path) -> IoResult<CommandOutput>;
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

    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>> {
        let mut entries: Vec<DirEntry> = Vec::new();
        for entry in read_dir(path)? {
            let entry: DirectoryEntry = entry?;
            let kind: FileKind = entry.file_type()?.into();
            entries.push(DirEntry::new(entry.path(), kind));
        }
        Ok(entries)
    }

    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>> {
        match symlink_metadata(path) {
            Ok(metadata) => Ok(Some(metadata.file_type().into())),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
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

    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>> {
        self.file_system.read_directory(path)
    }

    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>> {
        self.file_system.file_kind(path)
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

    fn run_command(&self, command: &ShellCommand, working_directory: &Path) -> IoResult<CommandOutput> {
        let mut parts: SplitAsciiWhitespace<'_> = command.as_ref().split_ascii_whitespace();
        let program: &str = parts.next().unwrap_or("");
        let arguments: Vec<&str> = parts.collect();
        let output: Output = Command::new(program)
            .args(&arguments)
            .current_dir(working_directory)
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
use std::cmp::Ordering;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::io::Error as IoError;

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

/// An in-memory [`Runtime`] for hermetic tests. Build one with [`DummyRuntime::builder`],
/// registering the files, directories, symlinks and canned command outputs a test needs. It also
/// implements [`FileSystem`] and [`Bootstrap`], so it can stand in anywhere from bootstrap onward.
#[cfg(test)]
pub struct DummyRuntime {
    files: HashMap<PathBuf, Vec<u8>>,
    directories: HashSet<PathBuf>,
    symlinks: HashSet<PathBuf>,
    commands: HashMap<String, CommandOutput>,
    current_directory: PathBuf,
    now: Instant,
    writes: Mutex<HashMap<PathBuf, Vec<u8>>>,
    created_directories: Mutex<HashSet<PathBuf>>,
    logs: Mutex<Vec<String>>,
    output: Mutex<Stdout>,
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
        Ok(self.current_directory.clone())
    }

    fn read_to_string(&self, path: &Path) -> IoResult<String> {
        match self.files.get(path) {
            Some(contents) => {
                String::from_utf8(contents.clone()).map_err(|error| IoError::new(ErrorKind::InvalidData, error))
            }
            None => Err(IoError::new(
                ErrorKind::NotFound,
                format!("no such file: {}", path.display()),
            )),
        }
    }

    fn read_directory(&self, path: &Path) -> IoResult<Vec<DirEntry>> {
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

    fn file_kind(&self, path: &Path) -> IoResult<Option<FileKind>> {
        Ok(self.kind_of(path))
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

    fn run_command(&self, command: &ShellCommand, _working_directory: &Path) -> IoResult<CommandOutput> {
        match self.commands.get(command.as_ref()) {
            Some(output) => Ok(output.clone()),
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
    files: HashMap<PathBuf, Vec<u8>>,
    directories: HashSet<PathBuf>,
    symlinks: HashSet<PathBuf>,
    commands: HashMap<String, CommandOutput>,
    current_directory: PathBuf,
    now: Instant,
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
            now: Instant::now(),
        }
    }

    /// Register a file with the given contents. Ancestor directories are created automatically.
    pub fn file(mut self, path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> DummyRuntimeBuilder {
        let path: PathBuf = path.as_ref().to_path_buf();
        self.register_ancestors(&path);
        self.files.insert(path, contents.as_ref().to_vec());
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
    pub fn command(mut self, command: impl Into<String>, output: CommandOutput) -> DummyRuntimeBuilder {
        self.commands.insert(command.into(), output);
        self
    }

    pub fn current_directory(mut self, path: impl AsRef<Path>) -> DummyRuntimeBuilder {
        self.current_directory = path.as_ref().to_path_buf();
        self
    }

    pub fn now(mut self, now: Instant) -> DummyRuntimeBuilder {
        self.now = now;
        self
    }

    pub fn build(self) -> DummyRuntime {
        DummyRuntime {
            files: self.files,
            directories: self.directories,
            symlinks: self.symlinks,
            commands: self.commands,
            current_directory: self.current_directory,
            now: self.now,
            writes: Mutex::new(HashMap::new()),
            created_directories: Mutex::new(HashSet::new()),
            logs: Mutex::new(Vec::new()),
            output: Mutex::new(Stdout::default()),
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
        let output: CommandOutput = runtime
            .run_command(&ShellCommand::new("go build"), Path::new("/workspace"))
            .unwrap();
        assert_eq!(output.stdout().as_bytes(), b"ok");
        assert_eq!(output.status(), TaskStatus::Succeeded);
        assert_eq!(
            runtime
                .run_command(&ShellCommand::new("go test"), Path::new("/workspace"))
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
}
