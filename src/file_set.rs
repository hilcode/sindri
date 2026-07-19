use crate::runtime::FileSystem;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use blake3::Hasher;
use globset::Glob;
use globset::GlobSet;
use globset::GlobSetBuilder;
use serde::Deserialize;
use serde::Serialize;
use smol_str::SmolStr;
use std::io::Result as IoResult;
use std::path::Path;
use std::path::PathBuf;

/// A field delimiter folded into the pattern hash between globs, so that two different splittings of
/// the same byte stream can never collide (e.g. `["a", "bc"]` versus `["ab", "c"]`).
const FIELD_SEPARATOR: [u8; 1] = [0];

/// An ordered list of include globs describing a set of files. It is a *description*, not a set of
/// files: resolving it against a directory yields a [`FileSet`]. A file is a member iff it matches at
/// least one glob, so an empty pattern selects nothing.
#[derive(Clone, Debug)]
pub struct FileSetPattern {
    globs: Vec<SmolStr>,
}

impl FileSetPattern {
    pub fn new(globs: impl IntoIterator<Item = impl Into<SmolStr>>) -> FileSetPattern {
        FileSetPattern {
            globs: globs.into_iter().map(|glob| glob.into()).collect(),
        }
    }

    pub fn globs(&self) -> &[SmolStr] {
        &self.globs
    }

    /// A digest over the glob strings, in list order. It changes whenever the pattern itself changes —
    /// a glob edited, added, or removed — which is the signal that a resolved file set must be
    /// recomputed, independently of whether the matched files changed.
    pub fn pattern_hash(&self) -> PatternHash {
        let mut hasher: Hasher = Hasher::new();
        for glob in &self.globs {
            hasher.update(glob.as_bytes());
            hasher.update(&FIELD_SEPARATOR);
        }
        PatternHash(*hasher.finalize().as_bytes())
    }

    /// Compile every glob into a single matcher once, so a walk can classify many paths without
    /// recompiling. An invalid glob string surfaces here rather than during traversal.
    pub fn compile(&self) -> Result<CompiledFileSetPattern, globset::Error> {
        let mut builder: GlobSetBuilder = GlobSetBuilder::new();
        for glob in &self.globs {
            builder.add(Glob::new(glob)?);
        }
        Ok(CompiledFileSetPattern {
            globs: builder.build()?,
        })
    }
}

/// A blake3 digest of a [`FileSetPattern`]'s ordered globs. Distinct from the other hash newtypes so
/// a pattern digest can never be compared against a file or file-set digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PatternHash([u8; 32]);

impl PatternHash {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// A [`FileSetPattern`] with its globs compiled into a single matcher, ready to classify the paths a
/// walk encounters.
pub struct CompiledFileSetPattern {
    globs: GlobSet,
}

impl CompiledFileSetPattern {
    /// Whether a file — given by its path relative to the walk's base — is a member of the set: true
    /// iff the path matches at least one of the pattern's globs.
    pub fn classify_file(&self, relative_path: &Path) -> bool {
        self.globs.is_match(relative_path)
    }
}

/// The concrete files selected by resolving a [`FileSetPattern`], each expressed relative to the
/// workspace root and held in a deterministic sorted order so a file set built from the same tree is
/// always identical regardless of the order the walk discovered its files in.
#[derive(Clone, Debug)]
pub struct FileSet {
    files: Vec<RelativeFile>,
}

impl FileSet {
    /// Resolve `pattern` against `base` — the directory the pattern's globs are anchored in and the
    /// root of the walk — expressing each selected file relative to the workspace `root`. The walk
    /// runs through `file_system`, so it is hermetic under a test runtime.
    pub fn resolve(
        pattern: &FileSetPattern,
        base: &AbsoluteDirectory,
        root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> IoResult<FileSet> {
        let matched: Vec<PathBuf> = file_system.matching_file_set(base.as_ref(), pattern)?;
        let files: Vec<RelativeFile> = matched
            .into_iter()
            .map(|path: PathBuf| -> RelativeFile { root.relativize_file(&AbsoluteFile::new(path)) })
            .collect();
        Ok(FileSet {
            files: sorted_deduplicated(files),
        })
    }

    /// The union of this file set with `other`, deterministically sorted like a resolved set. Used
    /// to combine a task's declared and managed inputs into its effective input.
    pub fn union(&self, other: &FileSet) -> FileSet {
        let files: Vec<RelativeFile> = self.files.iter().chain(other.files.iter()).cloned().collect();
        FileSet {
            files: sorted_deduplicated(files),
        }
    }

    pub fn files(&self) -> &[RelativeFile] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// A digest over each member file's raw content, read via `file_system` (relative to
    /// `workspace_root`) and folded in path order — the signal for whether a tracked file changed,
    /// appeared, or disappeared.
    pub fn content_hash(&self, workspace_root: &WorkspaceRoot, file_system: &impl FileSystem) -> IoResult<ContentHash> {
        let mut hasher: Hasher = Hasher::new();
        for file in &self.files {
            let absolute: AbsoluteFile = workspace_root.to_absolute_directory().join_file(file);
            let content: Vec<u8> = file_system.read(absolute.as_ref())?;
            hasher.update(file.as_ref().to_string_lossy().as_bytes());
            hasher.update(&FIELD_SEPARATOR);
            hasher.update(&content);
            hasher.update(&FIELD_SEPARATOR);
        }
        Ok(ContentHash(*hasher.finalize().as_bytes()))
    }
}

/// A blake3 digest of a resolved [`FileSet`]'s file contents, read byte-for-byte with no
/// normalization and folded in path order so the digest does not depend on resolution order.
/// Distinct from the other hash newtypes so a content digest can never be compared against a
/// pattern or binding digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentHash([u8; 32]);

/// Sort `files` and drop adjacent duplicates, giving the deterministic order every [`FileSet`] is
/// held in regardless of how its members were discovered or combined.
fn sorted_deduplicated(mut files: Vec<RelativeFile>) -> Vec<RelativeFile> {
    files.sort_by(|first: &RelativeFile, second: &RelativeFile| first.as_ref().cmp(second.as_ref()));
    files.dedup_by(|first: &mut RelativeFile, second: &mut RelativeFile| first.as_ref() == second.as_ref());
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::Bootstrap;
    use crate::runtime::DummyRuntime;
    use crate::runtime::Runtime;
    use crate::runtime::SystemFileSystem;
    use crate::types::BuildStart;
    use std::collections::BTreeSet;
    use std::fs::create_dir_all;
    use std::fs::write;
    use tempfile::TempDir;

    fn system_runtime() -> impl Runtime {
        SystemFileSystem.into_runtime(BuildStart::now(), None).unwrap()
    }

    fn resolved_paths(runtime: &DummyRuntime, base: &str, globs: &[&str]) -> BTreeSet<String> {
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(base)));
        let pattern: FileSetPattern = FileSetPattern::new(globs.iter().copied());
        let file_set: FileSet = FileSet::resolve(&pattern, &root.to_absolute_directory(), &root, runtime).unwrap();
        file_set
            .files()
            .iter()
            .map(|file: &RelativeFile| -> String { file.as_ref().display().to_string() })
            .collect()
    }

    #[test]
    fn a_pattern_exposes_its_globs_in_order() {
        let pattern: FileSetPattern = FileSetPattern::new(["**/*.go", "go.mod"]);
        assert_eq!(pattern.globs(), &[SmolStr::new("**/*.go"), SmolStr::new("go.mod")]);
    }

    #[test]
    fn classify_file_matches_a_file_against_any_glob() {
        let pattern: CompiledFileSetPattern = FileSetPattern::new(["**/*.go", "**/*.mod"]).compile().unwrap();
        assert!(pattern.classify_file(Path::new("main.go")));
        assert!(pattern.classify_file(Path::new("nested/helper.go")));
        assert!(pattern.classify_file(Path::new("go.mod")));
        assert!(!pattern.classify_file(Path::new("README.md")));
    }

    #[test]
    fn an_empty_pattern_selects_nothing() {
        let pattern: CompiledFileSetPattern = FileSetPattern::new(Vec::<&str>::new()).compile().unwrap();
        assert!(!pattern.classify_file(Path::new("main.go")));
    }

    #[test]
    fn resolving_yields_workspace_relative_paths_in_sorted_order() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/b.go", "")
            .file("/workspace/a.go", "")
            .file("/workspace/nested/c.go", "")
            .file("/workspace/notes.md", "")
            .build();
        let matched: BTreeSet<String> = resolved_paths(&runtime, "/workspace", &["**/*.go"]);
        assert_eq!(
            matched,
            BTreeSet::from(["a.go".to_string(), "b.go".to_string(), "nested/c.go".to_string()])
        );
    }

    #[test]
    fn resolving_selects_only_files_matched_by_the_pattern() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/main.go", "")
            .file("/workspace/bridge.c", "")
            .file("/workspace/README.md", "")
            .build();
        let matched: BTreeSet<String> = resolved_paths(&runtime, "/workspace", &["**/*.{go,c}"]);
        assert_eq!(matched, BTreeSet::from(["bridge.c".to_string(), "main.go".to_string()]));
    }

    #[test]
    fn a_file_set_reports_whether_it_is_empty_and_its_length() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.go", "")
            .file("/workspace/b.go", "")
            .build();
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let empty: FileSet = FileSet::resolve(
            &FileSetPattern::new(["**/*.rs"]),
            &root.to_absolute_directory(),
            &root,
            &runtime,
        )
        .unwrap();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        let non_empty: FileSet = FileSet::resolve(
            &FileSetPattern::new(["**/*.go"]),
            &root.to_absolute_directory(),
            &root,
            &runtime,
        )
        .unwrap();
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.len(), 2);
    }

    #[test]
    fn union_combines_and_deduplicates_two_file_sets_in_sorted_order() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.go", "")
            .file("/workspace/b.go", "")
            .file("/workspace/go.work", "")
            .build();
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let go_files: FileSet = FileSet::resolve(
            &FileSetPattern::new(["**/*.go"]),
            &root.to_absolute_directory(),
            &root,
            &runtime,
        )
        .unwrap();
        let go_work: FileSet = FileSet::resolve(
            &FileSetPattern::new(["go.work", "a.go"]),
            &root.to_absolute_directory(),
            &root,
            &runtime,
        )
        .unwrap();
        let union: BTreeSet<String> = go_files
            .union(&go_work)
            .files()
            .iter()
            .map(|file: &RelativeFile| -> String { file.as_ref().display().to_string() })
            .collect();
        assert_eq!(
            union,
            BTreeSet::from(["a.go".to_string(), "b.go".to_string(), "go.work".to_string()])
        );
    }

    #[test]
    fn content_hash_reproduces_for_the_same_file_set() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.go", "alpha")
            .file("/workspace/b.go", "beta")
            .build();
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let file_set: FileSet = FileSet::resolve(
            &FileSetPattern::new(["**/*.go"]),
            &root.to_absolute_directory(),
            &root,
            &runtime,
        )
        .unwrap();
        let first: ContentHash = file_set.content_hash(&root, &runtime).unwrap();
        let second: ContentHash = file_set.content_hash(&root, &runtime).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn content_hash_changes_when_a_tracked_files_content_changes() {
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let hash_with = |content: &str| -> ContentHash {
            let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", content).build();
            let file_set: FileSet = FileSet::resolve(
                &FileSetPattern::new(["**/*.go"]),
                &root.to_absolute_directory(),
                &root,
                &runtime,
            )
            .unwrap();
            file_set.content_hash(&root, &runtime).unwrap()
        };
        assert_ne!(hash_with("alpha"), hash_with("alpha!"));
    }

    #[test]
    fn content_hash_changes_when_a_file_is_added_or_removed() {
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let runtime_before: DummyRuntime = DummyRuntime::builder().file("/workspace/a.go", "alpha").build();
        let runtime_after: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.go", "alpha")
            .file("/workspace/b.go", "beta")
            .build();
        let hash_of = |runtime: &DummyRuntime| -> ContentHash {
            let file_set: FileSet = FileSet::resolve(
                &FileSetPattern::new(["**/*.go"]),
                &root.to_absolute_directory(),
                &root,
                runtime,
            )
            .unwrap();
            file_set.content_hash(&root, runtime).unwrap()
        };
        assert_ne!(hash_of(&runtime_before), hash_of(&runtime_after));
    }

    #[test]
    fn resolving_against_the_real_filesystem_walks_nested_and_normally_hidden_files() {
        let temporary_directory: TempDir = TempDir::new().unwrap();
        let root_path: PathBuf = temporary_directory.path().to_path_buf();
        create_dir_all(root_path.join("nested")).unwrap();
        write(root_path.join("main.go"), "").unwrap();
        write(root_path.join("nested/helper.go"), "").unwrap();
        // `standard_filters(false)` means the real walk is not pruned by the `ignore` crate's default
        // hidden-file and `.gitignore` filters — both a dotfile and a gitignored file must still be
        // matched, which a DummyRuntime's in-memory walk can never exercise.
        write(root_path.join(".hidden.go"), "").unwrap();
        write(root_path.join(".gitignore"), "ignored.go\n").unwrap();
        write(root_path.join("ignored.go"), "").unwrap();
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(root_path));
        let file_set: FileSet = FileSet::resolve(
            &FileSetPattern::new(["**/*.go"]),
            &root.to_absolute_directory(),
            &root,
            &system_runtime(),
        )
        .unwrap();
        let matched: BTreeSet<String> = file_set
            .files()
            .iter()
            .map(|file: &RelativeFile| -> String { file.as_ref().display().to_string() })
            .collect();
        assert_eq!(
            matched,
            BTreeSet::from([
                ".hidden.go".to_string(),
                "ignored.go".to_string(),
                "main.go".to_string(),
                "nested/helper.go".to_string(),
            ])
        );
    }

    #[test]
    fn the_pattern_hash_reflects_the_globs_and_reproduces_for_the_same_pattern() {
        let baseline: PatternHash = FileSetPattern::new(["**/*.go", "go.mod"]).pattern_hash();
        assert_eq!(baseline, FileSetPattern::new(["**/*.go", "go.mod"]).pattern_hash());
        assert_ne!(baseline, FileSetPattern::new(["**/*.rs", "go.mod"]).pattern_hash());
        assert_ne!(baseline, FileSetPattern::new(["**/*.go"]).pattern_hash());
    }
}
