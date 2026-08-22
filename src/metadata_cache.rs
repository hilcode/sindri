use crate::file_set::ContentHash;
use crate::file_set::FileSet;
use crate::runtime::FileSystem;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::FileMetadata;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use blake3::Hasher;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Error as IoError;
use std::io::ErrorKind;
use std::io::Result as IoResult;
use std::sync::Mutex;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

/// A field delimiter folded into the content hash between files, so that two different splittings of
/// the same byte stream can never collide (e.g. `["a", "bc"]` versus `["ab", "c"]`).
const FIELD_SEPARATOR: [u8; 1] = [0];

/// A single tracked file's content digest — a plain blake3 hash of its raw bytes. Private to this
/// module: nothing outside it needs a single file's hash, only [`MetadataCache::content_hash`]'s
/// fold of a whole [`FileSet`] into a [`ContentHash`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileContentHash([u8; 32]);

impl FileContentHash {
    fn of_bytes(bytes: &[u8]) -> FileContentHash {
        let mut hasher: Hasher = Hasher::new();
        hasher.update(bytes);
        FileContentHash(*hasher.finalize().as_bytes())
    }
}

/// A path's digest, used only to shard and locate its persisted record on disk — never compared
/// against a file's own content digest, so it stays a distinct type from [`FileContentHash`] even
/// though both happen to be blake3 output.
struct PathHash([u8; 32]);

impl PathHash {
    fn of(file: &RelativeFile) -> PathHash {
        let mut hasher: Hasher = Hasher::new();
        hasher.update(file.to_string().as_bytes());
        PathHash(*hasher.finalize().as_bytes())
    }

    /// Where this path's record lives under `cache_directory`: the first byte as a two-character
    /// subdirectory, the rest as the filename — spreading records evenly across a bounded number of
    /// subdirectories regardless of the real source tree's shape, and without mirroring that tree's
    /// structure (and so its file names) directly on disk.
    fn record_path(&self, cache_directory: &AbsoluteDirectory) -> AbsoluteFile {
        let hexadecimal: String = self
            .0
            .iter()
            .map(|byte: &u8| -> String { format!("{byte:02x}") })
            .collect();
        let (shard, rest): (&str, &str) = hexadecimal.split_at(2);
        cache_directory
            .join_file(&RelativeFile::new(format!("{shard}/{rest}.bin")).expect("a hex digest is always well-formed"))
    }
}

/// A file's persisted state as of the last time [`MetadataCache::persist`] wrote it: enough to tell,
/// via a cheap [`FileSystem::file_metadata`] call, whether the file might have changed since —
/// without reading its content unless that check is ambiguous.
#[derive(Serialize, Deserialize)]
struct PersistedRecord {
    size: u64,
    /// `modified`, expressed as whole seconds and the remaining nanoseconds since [`UNIX_EPOCH`] —
    /// `SystemTime` itself is not `(de)serializable` in a platform-independent way, and a build cache
    /// must survive being read back on a different machine than the one that wrote it.
    modified_seconds: u64,
    modified_nanos: u32,
    hash: [u8; 32],
}

impl PersistedRecord {
    fn new(metadata: FileMetadata, hash: FileContentHash) -> PersistedRecord {
        let since_epoch = metadata.modified().duration_since(UNIX_EPOCH).unwrap_or_default();
        PersistedRecord {
            size: metadata.size(),
            modified_seconds: since_epoch.as_secs(),
            modified_nanos: since_epoch.subsec_nanos(),
            hash: hash.0,
        }
    }

    fn modified(&self) -> SystemTime {
        UNIX_EPOCH + std::time::Duration::new(self.modified_seconds, self.modified_nanos)
    }

    fn hash(&self) -> FileContentHash {
        FileContentHash(self.hash)
    }

    /// Whether `current` still matches this record closely enough to trust its hash without reading
    /// the file's content: same size and the same modification time to the nanosecond.
    fn matches(&self, current: FileMetadata) -> bool {
        self.size == current.size() && self.modified() == current.modified()
    }
}

/// A per-build cache of file content hashes, backed by a small persisted record per file (under
/// `cache_directory`, one file per tracked path — never one large blob rewritten wholesale) so that
/// confirmed state survives a build getting interrupted before it finishes. Sindri assumes a tracked
/// file's content is stable for the life of one build: two resolved `FileSet`s hashed through
/// [`content_hash`](MetadataCache::content_hash) therefore read and hash a file they share at most
/// once between them, no matter how many dirtiness checks or run records consult it. A task whose own
/// commands write to a file — most notably its own output, which a dirtiness check may have already
/// cached as missing or stale — must [`invalidate`](MetadataCache::invalidate) that file itself;
/// nothing here detects the change on its own. That invalidation is deliberately "soft": it forces
/// the next read to be real rather than declaring the file dirty outright, so a later task in the
/// same build that only consumes this file (rather than running it) still sees an accurate,
/// up-to-date hash and can correctly skip itself if the content it actually depends on turns out
/// unchanged, even though the task that produced it did run. Multiple tasks may resolve their
/// content hashes at the same time, so access is `Mutex`-protected.
pub struct MetadataCache {
    cache_directory: AbsoluteDirectory,
    entries: Mutex<BTreeMap<RelativeFile, FileContentHash>>,
}

impl MetadataCache {
    pub fn new(cache_directory: AbsoluteDirectory) -> MetadataCache {
        MetadataCache {
            cache_directory,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// A digest over `files`' content, folded in path order — the signal for whether a tracked file
    /// changed, appeared, or disappeared. Each member file's hash is served from this cache rather
    /// than a direct read, so a file shared by more than one resolved set is read at most once per
    /// build; a file whose size and modification time still match its persisted record is trusted
    /// without being read at all.
    pub fn content_hash(
        &self,
        files: &FileSet,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> IoResult<ContentHash> {
        let mut hasher: Hasher = Hasher::new();
        for file in files.files() {
            let file_hash: FileContentHash = self.hash_of(file, workspace_root, file_system)?;
            hasher.update(file.to_string().as_bytes());
            hasher.update(&FIELD_SEPARATOR);
            hasher.update(&file_hash.0);
            hasher.update(&FIELD_SEPARATOR);
        }
        Ok(ContentHash::new(*hasher.finalize().as_bytes()))
    }

    /// Forget every file in `files`' memoized (this-build) hash — call this for every task that ran,
    /// on its own resolved output, the one thing its own commands are guaranteed to have touched. The
    /// next [`content_hash`](MetadataCache::content_hash) call for any of them reads and re-memoizes
    /// it fresh; this never declares a file dirty by itself, it only clears the way for the next
    /// reader to find out for real.
    pub fn invalidate(&self, files: &FileSet) {
        let mut entries = self.entries.lock().unwrap();
        for file in files.files() {
            entries.remove(file);
        }
    }

    /// Write `files`' current content hash and metadata as each one's new persisted record — call
    /// this once a task's resolved input and output are known to be settled (after it has finished
    /// running, or immediately for a task found clean), so a later build can trust them via a cheap
    /// [`FileSystem::file_metadata`] check instead of reading their content again.
    pub fn persist(
        &self,
        files: &FileSet,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> IoResult<()> {
        for file in files.files() {
            let hash: FileContentHash = self.hash_of(file, workspace_root, file_system)?;
            let absolute: AbsoluteFile = workspace_root.to_absolute_directory().join_file(file);
            let metadata: FileMetadata = file_system.file_metadata(absolute.as_ref())?;
            let record: PersistedRecord = PersistedRecord::new(metadata, hash);
            let record_path: AbsoluteFile = PathHash::of(file).record_path(&self.cache_directory);
            let bytes: Vec<u8> = rmp_serde::to_vec(&record).map_err(IoError::other)?;
            if let Some(parent) = record_path.parent() {
                file_system.create_directories(parent.as_ref())?;
            }
            file_system.write(record_path.as_ref(), &bytes)?;
        }
        Ok(())
    }

    /// `file`'s content hash: served from this build's own memo if already resolved, otherwise
    /// settled via [`resolve_hash`](MetadataCache::resolve_hash) and memoized for whoever asks next.
    fn hash_of(
        &self,
        file: &RelativeFile,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> IoResult<FileContentHash> {
        if let Some(hash) = self.entries.lock().unwrap().get(file) {
            return Ok(*hash);
        }
        let hash: FileContentHash = self.resolve_hash(file, workspace_root, file_system)?;
        self.entries.lock().unwrap().insert(file.clone(), hash);
        Ok(hash)
    }

    /// `file`'s current content hash, trusting its persisted record's hash without a read when a
    /// cheap [`FileSystem::file_metadata`] check confirms nothing about the file has changed since
    /// that record was written — reading its content only when there is no persisted record yet, or
    /// when the file's size or modification time no longer match it.
    fn resolve_hash(
        &self,
        file: &RelativeFile,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> IoResult<FileContentHash> {
        let absolute: AbsoluteFile = workspace_root.to_absolute_directory().join_file(file);
        let current_metadata: FileMetadata = file_system.file_metadata(absolute.as_ref())?;
        if let Some(record) = self.load_record(file, file_system)? {
            if record.matches(current_metadata) {
                return Ok(record.hash());
            }
        }
        let bytes: Vec<u8> = file_system.read(absolute.as_ref())?;
        Ok(FileContentHash::of_bytes(&bytes))
    }

    /// `file`'s persisted record, if it has one — a missing or unreadable (including corrupt or
    /// written by an incompatible format) record is treated the same as no record at all, so the
    /// caller simply falls back to reading the file's content.
    fn load_record(&self, file: &RelativeFile, file_system: &impl FileSystem) -> IoResult<Option<PersistedRecord>> {
        let record_path: AbsoluteFile = PathHash::of(file).record_path(&self.cache_directory);
        match file_system.read(record_path.as_ref()) {
            Ok(bytes) => Ok(rmp_serde::from_slice(&bytes).ok()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_set::FileSetPattern;
    use crate::runtime::DummyRuntime;
    use crate::types::AbsoluteDirectory;
    use std::path::PathBuf;
    use std::time::Duration;

    const WORKSPACE: &str = "/workspace";

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)))
    }

    fn cache_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace/.target/.metadata-cache"))
    }

    fn cache() -> MetadataCache {
        MetadataCache::new(cache_directory())
    }

    fn go_files(runtime: &DummyRuntime) -> FileSet {
        let root: WorkspaceRoot = workspace_root();
        FileSet::resolve(
            &FileSetPattern::new(["**/*.go"]),
            &root.to_absolute_directory(),
            &root,
            runtime,
        )
        .unwrap()
    }

    /// Replay `runtime`'s persisted records as real, readable files against `contents` — mirrors how
    /// a fresh build sees the previous build's writes, since a `DummyRuntime`'s own `write` log is
    /// invisible to its `read`.
    fn replay(runtime: &DummyRuntime, contents: DummyRuntimeBuilderExt) -> DummyRuntime {
        let mut builder = contents;
        for (path, bytes) in runtime.written_files() {
            builder = builder.file(path, bytes);
        }
        builder.build()
    }

    type DummyRuntimeBuilderExt = crate::runtime::DummyRuntimeBuilder;

    #[test]
    fn content_hash_reads_an_unseen_file() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", "alpha").build();
        cache()
            .content_hash(&go_files(&runtime), &workspace_root(), &runtime)
            .unwrap();
        assert_eq!(runtime.read_count("/workspace/a.go"), 1);
    }

    #[test]
    fn content_hash_surfaces_an_io_error_for_a_file_missing_from_the_runtime() {
        // Resolve the file set against a runtime that has it, then hash it against one that doesn't
        // — the file vanishing between the two is exactly what a real deletion between passes would
        // look like, and it should surface as an error rather than panic or silently skip the file.
        let with_file: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", "alpha").build();
        let files: FileSet = go_files(&with_file);
        let without_file: DummyRuntime = DummyRuntime::builder().build();
        let result: IoResult<ContentHash> = cache().content_hash(&files, &workspace_root(), &without_file);
        assert!(result.is_err());
    }

    #[test]
    fn a_second_call_for_the_same_file_set_does_not_read_again() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", "alpha").build();
        let cache: MetadataCache = cache();
        let files: FileSet = go_files(&runtime);
        let first: ContentHash = cache.content_hash(&files, &workspace_root(), &runtime).unwrap();
        let second: ContentHash = cache.content_hash(&files, &workspace_root(), &runtime).unwrap();
        assert_eq!(runtime.read_count("/workspace/a.go"), 1);
        assert_eq!(first, second);
    }

    #[test]
    fn two_overlapping_file_sets_share_one_read_of_the_common_file() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.go", "alpha")
            .file("/workspace/b.go", "beta")
            .build();
        let cache: MetadataCache = cache();
        cache
            .content_hash(&go_files(&runtime), &workspace_root(), &runtime)
            .unwrap();
        cache
            .content_hash(&go_files(&runtime), &workspace_root(), &runtime)
            .unwrap();
        assert_eq!(runtime.read_count("/workspace/a.go"), 1);
        assert_eq!(runtime.read_count("/workspace/b.go"), 1);
    }

    #[test]
    fn content_hash_changes_when_a_tracked_files_content_changes() {
        let hash_with = |content: &str| -> ContentHash {
            let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", content).build();
            cache()
                .content_hash(&go_files(&runtime), &workspace_root(), &runtime)
                .unwrap()
        };
        assert_ne!(hash_with("alpha"), hash_with("alpha!"));
    }

    #[test]
    fn invalidate_forces_the_next_content_hash_call_to_read_again() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", "alpha").build();
        let cache: MetadataCache = cache();
        cache
            .content_hash(&go_files(&runtime), &workspace_root(), &runtime)
            .unwrap();
        cache.invalidate(&go_files(&runtime));
        cache
            .content_hash(&go_files(&runtime), &workspace_root(), &runtime)
            .unwrap();
        assert_eq!(runtime.read_count("/workspace/a.go"), 2);
    }

    #[test]
    fn a_file_matching_its_persisted_size_and_mtime_is_trusted_without_a_read() {
        let modified: SystemTime = UNIX_EPOCH + Duration::from_secs(1000);
        let first_build: DummyRuntime = DummyRuntime::builder()
            .file_modified_at("/workspace/a.go", "alpha", modified)
            .build();
        let files: FileSet = go_files(&first_build);
        cache().persist(&files, &workspace_root(), &first_build).unwrap();

        let second_build: DummyRuntime = replay(
            &first_build,
            DummyRuntime::builder().file_modified_at("/workspace/a.go", "alpha", modified),
        );
        cache()
            .content_hash(&go_files(&second_build), &workspace_root(), &second_build)
            .unwrap();
        assert_eq!(second_build.read_count("/workspace/a.go"), 0);
    }

    #[test]
    fn a_file_whose_size_changed_is_read_without_consulting_mtime() {
        let modified: SystemTime = UNIX_EPOCH + Duration::from_secs(1000);
        let first_build: DummyRuntime = DummyRuntime::builder()
            .file_modified_at("/workspace/a.go", "alpha", modified)
            .build();
        let files: FileSet = go_files(&first_build);
        cache().persist(&files, &workspace_root(), &first_build).unwrap();

        // Same mtime, different (longer) content: genuinely possible, not just a contrived case —
        // some VCS/checkout tools set every checked-out file's mtime to the commit or checkout time
        // rather than to when its content actually last changed, so switching states can easily leave
        // two different-content files sharing one mtime. Exactly what proves the size check alone is
        // enough to force a read here.
        let second_build: DummyRuntime = replay(
            &first_build,
            DummyRuntime::builder().file_modified_at("/workspace/a.go", "alpha-longer", modified),
        );
        cache()
            .content_hash(&go_files(&second_build), &workspace_root(), &second_build)
            .unwrap();
        assert_eq!(second_build.read_count("/workspace/a.go"), 1);
    }

    #[test]
    fn a_touched_but_unedited_file_is_read_once_and_then_trusted_again() {
        let original: SystemTime = UNIX_EPOCH + Duration::from_secs(1000);
        let touched: SystemTime = UNIX_EPOCH + Duration::from_secs(2000);
        let first_build: DummyRuntime = DummyRuntime::builder()
            .file_modified_at("/workspace/a.go", "alpha", original)
            .build();
        let files: FileSet = go_files(&first_build);
        cache().persist(&files, &workspace_root(), &first_build).unwrap();

        // Same content, only the mtime moved forward: ambiguous from size alone, so this build must
        // read once to resolve it — but re-persisting settles the record for the build after that.
        let second_build: DummyRuntime = replay(
            &first_build,
            DummyRuntime::builder().file_modified_at("/workspace/a.go", "alpha", touched),
        );
        let second_cache: MetadataCache = cache();
        second_cache
            .content_hash(&go_files(&second_build), &workspace_root(), &second_build)
            .unwrap();
        assert_eq!(second_build.read_count("/workspace/a.go"), 1);
        second_cache
            .persist(&go_files(&second_build), &workspace_root(), &second_build)
            .unwrap();

        let third_build: DummyRuntime = replay(
            &second_build,
            DummyRuntime::builder().file_modified_at("/workspace/a.go", "alpha", touched),
        );
        cache()
            .content_hash(&go_files(&third_build), &workspace_root(), &third_build)
            .unwrap();
        assert_eq!(third_build.read_count("/workspace/a.go"), 0);
    }

    #[test]
    fn persist_writes_one_small_record_per_file_not_one_shared_blob() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.go", "alpha")
            .file("/workspace/b.go", "beta")
            .build();
        let files: FileSet = go_files(&runtime);
        cache().persist(&files, &workspace_root(), &runtime).unwrap();
        let written: Vec<PathBuf> = runtime.written_files().into_iter().map(|(path, _)| path).collect();
        assert_eq!(written.len(), 2, "one record per tracked file, not a shared cache blob");
        assert!(written.iter().all(|path: &PathBuf| -> bool {
            path.starts_with("/workspace/.target/.metadata-cache")
                && path.extension().is_some_and(|extension| extension == "bin")
        }));
    }

    #[test]
    fn loading_a_missing_record_is_treated_as_no_record() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", "alpha").build();
        let record: Option<PersistedRecord> = cache()
            .load_record(&RelativeFile::new_unchecked("a.go"), &runtime)
            .unwrap();
        assert!(record.is_none());
    }
}
