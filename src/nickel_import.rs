use crate::error::SindriError;
use crate::error::SindriResult;
use crate::runtime::FileSystem;
use crate::types::AbsoluteFile;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use nickel_lang_core::cache::CacheHub;
use nickel_lang_core::cache::ImportResolver;
use nickel_lang_core::cache::InputFormat;
use nickel_lang_core::cache::ResolvedTerm;
use nickel_lang_core::cache::SourcePath;
use nickel_lang_core::cache::TermEntry;
use nickel_lang_core::cache::normalize_path;
use nickel_lang_core::error::Error as NickelError;
use nickel_lang_core::error::ImportError;
use nickel_lang_core::error::ImportErrorKind;
use nickel_lang_core::error::NullReporter;
use nickel_lang_core::error::ParseErrors;
use nickel_lang_core::error::report::ColorOpt;
use nickel_lang_core::error::report::report_as_str;
use nickel_lang_core::eval::VirtualMachine;
use nickel_lang_core::eval::VmContext;
use nickel_lang_core::eval::cache::CacheImpl;
use nickel_lang_core::eval::value::NickelValue;
use nickel_lang_core::files::FileId;
use nickel_lang_core::files::Files;
use nickel_lang_core::position::PosIdx;
use nickel_lang_core::position::PosTable;
use nickel_lang_core::position::TermPos;
use nickel_lang_core::term::Import;
use nickel_lang_core::transform::import_resolution::strict::ResolveResult;
use nickel_lang_core::transform::import_resolution::strict::resolve_imports;
use nickel_lang_core::typ::UnboundTypeVariableError;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::sink;
use std::marker::PhantomData;
use std::path::Path;
use std::path::PathBuf;

/// Marker types distinguishing which resolution pass a [`TypedCacheHub`] belongs to. Never
/// constructed — used only as a type parameter.
enum HashPass {}
enum EvalPass {}

/// A `CacheHub` tagged at the type level with the pass it serves, so the definition-hash hub and
/// the evaluation hub — otherwise identically-typed `nickel_lang_core` values — can never be
/// swapped by mistake despite [`ScriptResolutionState`] holding one of each. Paired with its own
/// `PosTable`: a cached term's positions are indices into whichever table was live when the term
/// was parsed, so once a hub is long-lived, its position table must persist alongside it too —
/// resolving a later call against a fresh table would leave earlier, still-cached terms pointing
/// at indices the fresh table never allocated.
struct TypedCacheHub<Pass> {
    hub: CacheHub,
    pos_table: PosTable,
    marker: PhantomData<Pass>,
}

impl<Pass> TypedCacheHub<Pass> {
    fn new() -> TypedCacheHub<Pass> {
        TypedCacheHub {
            hub: CacheHub::new(),
            pos_table: PosTable::new(),
            marker: PhantomData,
        }
    }
}

/// The raw text of a Nickel source file, read once and shared between the hash-pass and eval-pass
/// hubs so the disk read itself happens at most once per file per compile even though each hub
/// still parses its own copy.
struct NickelSource(String);

/// The state every task's script is resolved against for a whole `sindri compile`: a
/// definition-hash hub and an evaluation hub, kept as two distinct types (see [`TypedCacheHub`])
/// because a task's hash-document and eval-document share one synthetic script path, and
/// `SourceCache::add_string` does not check for an existing entry under the same path — merging
/// the two would silently collide. `file_contents` is the disk-read dedup layer shared *between*
/// the two hubs: each hub still parses its own copy of a file (a CPU-only cost), but the underlying
/// read happens once. Confined to this module — every other module only ever sees `&mut
/// ScriptResolutionState`, so a Nickel type can never leak into code that does not need one.
pub struct ScriptResolutionState {
    hash_hub: TypedCacheHub<HashPass>,
    eval_hub: TypedCacheHub<EvalPass>,
    file_contents: HashMap<AbsoluteFile, NickelSource>,
}

impl ScriptResolutionState {
    pub fn new() -> ScriptResolutionState {
        ScriptResolutionState {
            hash_hub: TypedCacheHub::new(),
            eval_hub: TypedCacheHub::new(),
            file_contents: HashMap::new(),
        }
    }
}

/// Every Nickel source a script's evaluation transitively depends on: the script itself and every
/// file it imports, directly or through further imports. Obtained by [`resolve_transitive_source`],
/// which resolves imports through Sindri's [`FileSystem`] rather than the real disk, so the set is
/// exactly what [`ImportResolver`] discovers — nothing pre-seeded, nothing left unaccounted for.
/// Paths are relative to the workspace root, not absolute, so a definition hash computed from this
/// set is the same regardless of where the workspace itself is checked out.
#[derive(Clone, Debug)]
pub struct TransitiveSource {
    files: Vec<(RelativeFile, String)>,
}

impl TransitiveSource {
    pub fn files(&self) -> &[(RelativeFile, String)] {
        &self.files
    }
}

/// Resolve `source` — a script's Nickel expression, addressed by `source_path` for the purpose of
/// resolving its own relative imports — together with every file it transitively imports. Each
/// import is read through `file_system`, never the real disk, and one resolving outside
/// `workspace_root` is a build-definition error rather than a silent read of an unrelated file.
pub fn resolve_transitive_source(
    source: &str,
    source_path: &AbsoluteFile,
    workspace_root: &WorkspaceRoot,
    resolution_state: &mut ScriptResolutionState,
    file_system: &impl FileSystem,
) -> SindriResult<TransitiveSource> {
    let (_main_file_id, reached): (FileId, HashSet<FileId>) = resolve_hermetically(
        source,
        source_path,
        workspace_root,
        &mut resolution_state.hash_hub.hub,
        &mut resolution_state.hash_hub.pos_table,
        &mut resolution_state.file_contents,
        file_system,
    )?;
    let hub: &CacheHub = &resolution_state.hash_hub.hub;
    let mut files: Vec<(RelativeFile, String)> = reached
        .into_iter()
        .filter_map(|file_id: FileId| -> Option<(RelativeFile, String)> {
            let source_path: &SourcePath = hub.sources.file_paths.get(&file_id)?;
            transitive_source_entry(file_id, source_path, hub, workspace_root)
        })
        .collect();
    files.sort_by(|first: &(RelativeFile, String), second: &(RelativeFile, String)| {
        first.0.as_ref().cmp(second.0.as_ref())
    });
    Ok(TransitiveSource { files })
}

/// The `(path, source text)` pair `file_id`/`source_path` contributes to a [`TransitiveSource`], or
/// `None` if `source_path` isn't a real file. Every other [`SourcePath`] variant — the stdlib, a REPL
/// snippet, a CLI field assignment, ... — has no file to report and is excluded deliberately, not
/// merely unhandled; `resolve_hermetically`'s own resolution never produces one of these for a task
/// script, but the match stays exhaustive over every variant `SourcePath` could ever be. The path is
/// reported relative to `workspace_root` so a definition hash built from it doesn't depend on where
/// the workspace itself is checked out.
fn transitive_source_entry(
    file_id: FileId,
    source_path: &SourcePath,
    hub: &CacheHub,
    workspace_root: &WorkspaceRoot,
) -> Option<(RelativeFile, String)> {
    match source_path {
        SourcePath::Path(path, _) => Some((
            workspace_root.relativize_file(&AbsoluteFile::new(path.clone())),
            hub.sources.source(file_id).to_string(),
        )),
        _ => None,
    }
}

/// Fully evaluate `source` hermetically: resolve its transitive imports (see
/// [`resolve_transitive_source`]) and deeply evaluate the result via `nickel-lang-core`'s own
/// virtual machine, standard library included. This is the one evaluation path Sindri uses for a
/// task's `Script`, so a script's imports work identically whether Sindri is computing its
/// definition hash or actually running it.
pub fn evaluate_hermetically(
    source: &str,
    source_path: &AbsoluteFile,
    workspace_root: &WorkspaceRoot,
    resolution_state: &mut ScriptResolutionState,
    file_system: &impl FileSystem,
) -> SindriResult<NickelValue> {
    let hub: &mut CacheHub = &mut resolution_state.eval_hub.hub;
    let pos_table: &mut PosTable = &mut resolution_state.eval_hub.pos_table;
    let (main_file_id, _reached): (FileId, HashSet<FileId>) = resolve_hermetically(
        source,
        source_path,
        workspace_root,
        hub,
        pos_table,
        &mut resolution_state.file_contents,
        file_system,
    )?;
    hub.prepare_stdlib(pos_table)
        .map_err(|error: NickelError| -> SindriError { import_resolution_error(&mut hub.sources.files, error) })?;
    let main_value: NickelValue = hub
        .terms
        .get_owned(main_file_id)
        .expect("main_file_id was resolved by resolve_hermetically");
    // `eval_full` evaluates whatever `pending_contracts` a term already carries rather than deriving
    // them itself; this is the transform (`gen_pending_contracts`) that `nickel_lang::Context::eval_deep`
    // runs internally via `prepare_eval`. Skipping it here would turn every field contract into a no-op.
    let main_value: NickelValue = nickel_lang_core::transform::transform(pos_table, main_value, None)
        .map_err(|error| -> SindriError { transform_error(&mut hub.sources.files, error) })?;
    // `VmContext` owns its import resolver and position table by value rather than borrowing them,
    // so the long-lived hub and its position table are moved in for the duration of evaluation and
    // reclaimed afterwards, keeping whatever they accumulated available to the next
    // `evaluate_hermetically` call that reuses this same state.
    let owned_hub: CacheHub = std::mem::replace(hub, CacheHub::new());
    let owned_pos_table: PosTable = std::mem::replace(pos_table, PosTable::new());
    let mut vm_context: VmContext<CacheHub, CacheImpl> =
        VmContext::new_with_pos_table(owned_hub, owned_pos_table, sink(), NullReporter {});
    let mut vm: VirtualMachine<'_, CacheHub, CacheImpl> = VirtualMachine::new(&mut vm_context);
    let result: Result<NickelValue, nickel_lang_core::error::EvalError> = vm.eval_full(main_value);
    // `VirtualMachine` implements `Drop`, which extends its mutable borrow of `vm_context` to the end
    // of scope regardless of last use — dropping it explicitly frees `vm_context` up to reclaim the
    // hub and position table back out, on both the success and error paths.
    drop(vm);
    *hub = vm_context.import_resolver;
    *pos_table = vm_context.pos_table;
    result.map_err(|error| -> SindriError { import_resolution_error(&mut hub.sources.files, error) })
}

/// Hermetically resolve `source`'s entire transitive import closure into `hub`, addressed as if it
/// lived at `source_path`. Every reached file's cached term has its imports already replaced by
/// `ResolvedImport` markers, so `hub` is ready to be evaluated as-is. `hub` is caller-owned —
/// typically a [`ScriptResolutionState`]'s long-lived hash-pass or eval-pass hub — so imports
/// already resolved by an earlier call are served from `hub.sources.id_of`'s cache hit instead of
/// being re-read. Returns the main file's id alongside exactly the set of `FileId`s reached from
/// it — the whole hub may hold far more once it is shared across many calls (one per task), and a
/// caller building a definition hash must scope to its own task's files, not the hub's entire
/// contents (see [`resolve_transitive_source`]).
fn resolve_hermetically(
    source: &str,
    source_path: &AbsoluteFile,
    workspace_root: &WorkspaceRoot,
    hub: &mut CacheHub,
    pos_table: &mut PosTable,
    file_contents: &mut HashMap<AbsoluteFile, NickelSource>,
    file_system: &impl FileSystem,
) -> SindriResult<(FileId, HashSet<FileId>)> {
    let main_source_path: SourcePath = SourcePath::Path(source_path.as_ref().to_path_buf(), InputFormat::Nickel);
    let main_file_id: FileId = hub.sources.add_string(main_source_path, source.to_string());
    hub.parse_to_term(pos_table, main_file_id, InputFormat::Nickel)
        .map_err(|parse_errors: ParseErrors| -> SindriError {
            import_resolution_error(&mut hub.sources.files, parse_errors)
        })?;
    let mut resolver: WorkspaceImportResolver<'_, _> = WorkspaceImportResolver {
        hub,
        workspace_root,
        file_system,
        file_contents,
    };
    let mut reached: HashSet<FileId> = HashSet::new();
    resolve_transitively(pos_table, &mut resolver, main_file_id, &mut reached).map_err(
        |import_error: ImportError| -> SindriError {
            import_resolution_error(&mut resolver.hub.sources.files, import_error)
        },
    )?;
    Ok((main_file_id, reached))
}

/// Render `error` through nickel-lang-core's own diagnostic reporter — the same span-aware, snippet-
/// printing machinery `nickel export` uses — rather than its `Debug` output, so a build-definition
/// error points at the offending span in the script's own source instead of dumping the error's
/// internal representation.
fn import_resolution_error(files: &mut Files, error: impl Into<NickelError>) -> SindriError {
    SindriError::ScriptEvaluation {
        nickel_message: report_as_str(files, error.into(), ColorOpt::Never),
    }
}

/// The one failure [`nickel_lang_core::transform::transform`] can produce: a type annotation
/// referencing a variable no `forall` bound. Reported through the same diagnostics path as every
/// other error here, via the conversion `nickel_lang_core::cache`'s own `prepare_impl` uses for this
/// exact error.
fn transform_error(files: &mut Files, error: UnboundTypeVariableError) -> SindriError {
    import_resolution_error(files, NickelError::ParseErrors(ParseErrors::from(error)))
}

/// Resolve `file_id`'s direct imports through `resolver`, persist the transformed term — its
/// `Term::Import` nodes replaced by `Term::ResolvedImport` markers — back into the term cache so a
/// later evaluation sees them rather than the original unresolved imports, then recurse into each
/// newly discovered file so the whole transitive closure is resolved. Mirrors the shape of
/// [`nickel_lang_core::cache::CacheHub::resolve_imports`], since that convenience method is
/// hard-coded to `CacheHub` as its own resolver and cannot be reused with a different one. `reached`
/// records every `file_id` visited by this call — both the reachable-set this call's caller needs,
/// and the guard that stops this recursion from re-walking a file's already-resolved subtree: once
/// `hub` is shared across many calls (one per task), `resolve_imports` reports every
/// `ResolvedImport` node in a term, including ones a *previous* call already resolved, not just
/// newly discovered ones.
fn resolve_transitively<FS: FileSystem>(
    pos_table: &mut PosTable,
    resolver: &mut WorkspaceImportResolver<'_, FS>,
    file_id: FileId,
    reached: &mut HashSet<FileId>,
) -> Result<(), ImportError> {
    if !reached.insert(file_id) {
        return Ok(());
    }
    let entry: TermEntry = resolver
        .hub
        .terms
        .get_entry(file_id)
        .cloned()
        .expect("file_id was parsed before resolving its imports");
    let result: ResolveResult = resolve_imports(pos_table, entry.value, resolver)?;
    resolver.hub.terms.insert(
        file_id,
        TermEntry {
            value: result.transformed_term,
            ..entry
        },
    );
    for resolved_id in result.resolved_ids {
        resolve_transitively(pos_table, resolver, resolved_id, reached)?;
    }
    Ok(())
}

/// An [`ImportResolver`] that resolves every import through Sindri's [`FileSystem`] instead of the
/// real disk, and rejects one resolving outside `workspace_root` before ever attempting to read it.
/// Borrows a [`CacheHub`] for source/term storage and delegates `files`/`get`/`get_path` to it
/// unchanged — only `resolve` differs from `CacheHub`'s own behaviour. Borrowed rather than owned
/// so a caller's long-lived hub (see [`ScriptResolutionState`]) keeps accumulating state across many
/// calls instead of being rebuilt fresh each time. `file_contents` is shared with the *other* hub's
/// resolver too (hash-pass and eval-pass), so a file already read by one pass is served to the other
/// without a second disk read — each hub still parses its own copy from the shared text.
struct WorkspaceImportResolver<'runtime, FS: FileSystem> {
    hub: &'runtime mut CacheHub,
    workspace_root: &'runtime WorkspaceRoot,
    file_system: &'runtime FS,
    file_contents: &'runtime mut HashMap<AbsoluteFile, NickelSource>,
}

impl<FS: FileSystem> ImportResolver for WorkspaceImportResolver<'_, FS> {
    fn resolve(
        &mut self,
        pos_table: &mut PosTable,
        import: &Import,
        parent: Option<FileId>,
        pos_idx: PosIdx,
    ) -> Result<(ResolvedTerm, FileId), ImportError> {
        let position: TermPos = pos_table.get(pos_idx);
        let Import::Path { path, format } = import else {
            return Err(Box::new(ImportErrorKind::IOError(
                "<package>".to_string(),
                "Sindri task scripts do not support package imports".to_string(),
                position,
            )));
        };
        let mut parent_directory: PathBuf = parent
            .and_then(|parent_id: FileId| -> Option<&OsStr> { self.hub.get_path(parent_id) })
            .map(PathBuf::from)
            .unwrap_or_default();
        parent_directory.pop();
        let joined: PathBuf = parent_directory.join(Path::new(path));
        let normalized: PathBuf = normalize_path(&joined).map_err(|io_error: std::io::Error| -> ImportError {
            import_io_error(path.to_string_lossy().into_owned(), position, io_error)
        })?;
        if !normalized.starts_with(self.workspace_root.as_ref()) {
            return Err(Box::new(ImportErrorKind::IOError(
                path.to_string_lossy().into_owned(),
                format!(
                    "import resolves to `{}`, which is outside the workspace root `{}`",
                    normalized.display(),
                    self.workspace_root
                ),
                position,
            )));
        }
        let source_path: SourcePath = SourcePath::Path(normalized.clone(), *format);
        let (file_id, resolved_term): (FileId, ResolvedTerm) = match self.hub.sources.id_of(&source_path) {
            Some(cached_id) => (cached_id, ResolvedTerm::FromCache),
            None => {
                let absolute: AbsoluteFile = AbsoluteFile::new(normalized.clone());
                let content: String = match self.file_contents.get(&absolute) {
                    Some(NickelSource(cached)) => cached.clone(),
                    None => {
                        let read: String = self.file_system.read_to_string(&normalized).map_err(
                            |io_error: std::io::Error| -> ImportError {
                                import_io_error(path.to_string_lossy().into_owned(), position, io_error)
                            },
                        )?;
                        self.file_contents.insert(absolute, NickelSource(read.clone()));
                        read
                    }
                };
                let file_id: FileId = self.hub.sources.add_string(source_path, content);
                (file_id, ResolvedTerm::FromFile { path: normalized })
            }
        };
        self.hub
            .parse_to_term(pos_table, file_id, *format)
            .map_err(|parse_errors| -> ImportError {
                Box::new(ImportErrorKind::ParseErrors(parse_errors, position))
            })?;
        Ok((resolved_term, file_id))
    }

    fn files(&self) -> &Files {
        self.hub.files()
    }

    fn get(&self, file_id: FileId) -> Option<NickelValue> {
        self.hub.get(file_id)
    }

    fn get_path(&self, file_id: FileId) -> Option<&OsStr> {
        self.hub.get_path(file_id)
    }
}

/// Wrap an I/O failure encountered while resolving `path` — normalizing it or reading its
/// content — as the [`ImportError`] shape `WorkspaceImportResolver::resolve` reports for both.
fn import_io_error(path: String, position: TermPos, io_error: std::io::Error) -> ImportError {
    Box::new(ImportErrorKind::IOError(path, io_error.to_string(), position))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::runtime::SystemFileSystem;
    use crate::types::AbsoluteDirectory;
    use nickel_lang_core::identifier::LocIdent;
    use serde::Deserialize;
    use std::fs::create_dir_all;
    use std::fs::write;
    use std::io::Error as IoError;
    use tempfile::TempDir;

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")))
    }

    fn source_path() -> AbsoluteFile {
        AbsoluteFile::new(PathBuf::from("/workspace/main.ncl"))
    }

    #[test]
    fn transitive_source_entry_reports_a_real_file() {
        let mut hub: CacheHub = CacheHub::new();
        let source_path: SourcePath = SourcePath::Path(PathBuf::from("/workspace/main.ncl"), InputFormat::Nickel);
        let file_id: FileId = hub.sources.add_string(source_path.clone(), "{}".to_string());
        let (file, contents): (RelativeFile, String) =
            transitive_source_entry(file_id, &source_path, &hub, &workspace_root())
                .expect("a `Path` source is a real file");
        assert_eq!(file.as_ref(), Path::new("main.ncl"));
        assert_eq!(contents, "{}");
    }

    #[test]
    fn transitive_source_entry_ignores_a_source_with_no_file() {
        let mut hub: CacheHub = CacheHub::new();
        let file_id: FileId = hub.sources.add_string(SourcePath::Query, "{}".to_string());
        assert!(transitive_source_entry(file_id, &SourcePath::Query, &hub, &workspace_root()).is_none());
    }

    #[test]
    fn transform_error_reports_the_unbound_identifier() {
        let mut files: Files = Files::empty();
        let error: SindriError = transform_error(&mut files, UnboundTypeVariableError(LocIdent::from("a")));
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        assert!(error.to_string().contains("unbound"), "message was: {error}");
        assert!(error.to_string().contains('a'), "message was: {error}");
    }

    #[test]
    fn import_io_error_carries_the_offending_path_and_the_io_message() {
        let error: ImportError = import_io_error("some/path".to_string(), TermPos::None, IoError::other("boom"));
        match *error {
            ImportErrorKind::IOError(path, message, position) => {
                assert_eq!(path, "some/path");
                assert!(message.contains("boom"), "message was: {message}");
                assert_eq!(position, TermPos::None);
            }
            other => panic!("expected ImportErrorKind::IOError, got {other:?}"),
        }
    }

    #[test]
    fn a_file_shared_by_the_hash_pass_and_the_eval_pass_is_read_from_disk_once() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/helper.ncl", "{ answer = 42 }")
            .build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        resolve_transitive_source(
            "import \"helper.ncl\"",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        evaluate_hermetically(
            "(import \"helper.ncl\").answer",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        assert_eq!(runtime.read_count("/workspace/helper.ncl"), 1);
    }

    #[test]
    fn evaluate_hermetically_evaluates_a_self_contained_script() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let value: NickelValue = evaluate_hermetically(
            "21 + 21",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        let answer: i64 = i64::deserialize(value).unwrap();
        assert_eq!(answer, 42);
    }

    #[test]
    fn evaluate_hermetically_evaluates_an_imported_script() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/helper.ncl", "21").build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let value: NickelValue = evaluate_hermetically(
            "(import \"helper.ncl\") + 21",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        let answer: i64 = i64::deserialize(value).unwrap();
        assert_eq!(answer, 42);
    }

    #[test]
    fn a_workspace_local_import_is_included() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/helper.ncl", "{ answer = 42 }")
            .build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let transitive: TransitiveSource = resolve_transitive_source(
            "import \"helper.ncl\"",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        let paths: Vec<PathBuf> = transitive
            .files()
            .iter()
            .map(|(file, _): &(RelativeFile, String)| -> PathBuf { file.as_ref().to_path_buf() })
            .collect();
        assert!(paths.contains(&PathBuf::from("main.ncl")), "paths were: {paths:?}");
        assert!(paths.contains(&PathBuf::from("helper.ncl")), "paths were: {paths:?}");
    }

    #[test]
    fn a_shared_hash_pass_hub_scopes_each_call_to_its_own_reachable_files() {
        // Simulates two tasks resolving their definition hash against the same long-lived hash-pass
        // hub (see `ScriptResolutionState`): each has its own script path and its own import, and
        // neither's `TransitiveSource` should see the other's files, even though both now live in
        // the same hub.
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/helper-a.ncl", "{ answer = 1 }")
            .file("/workspace/helper-b.ncl", "{ answer = 2 }")
            .build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let task_a_path: AbsoluteFile = AbsoluteFile::new(PathBuf::from("/workspace/task-a.ncl"));
        let task_b_path: AbsoluteFile = AbsoluteFile::new(PathBuf::from("/workspace/task-b.ncl"));
        resolve_transitive_source(
            "import \"helper-a.ncl\"",
            &task_a_path,
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        let task_b: TransitiveSource = resolve_transitive_source(
            "import \"helper-b.ncl\"",
            &task_b_path,
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        let task_b_paths: Vec<PathBuf> = task_b
            .files()
            .iter()
            .map(|(file, _): &(RelativeFile, String)| -> PathBuf { file.as_ref().to_path_buf() })
            .collect();
        assert!(
            task_b_paths.contains(&PathBuf::from("task-b.ncl")),
            "paths were: {task_b_paths:?}"
        );
        assert!(
            task_b_paths.contains(&PathBuf::from("helper-b.ncl")),
            "paths were: {task_b_paths:?}"
        );
        assert!(
            !task_b_paths.contains(&PathBuf::from("task-a.ncl")),
            "task B's transitive source should not include task A's own script: {task_b_paths:?}"
        );
        assert!(
            !task_b_paths.contains(&PathBuf::from("helper-a.ncl")),
            "task B's transitive source should not include task A's helper: {task_b_paths:?}"
        );
    }

    #[test]
    fn nested_imports_are_resolved_transitively() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.ncl", "import \"b.ncl\"")
            .file("/workspace/b.ncl", "import \"c.ncl\"")
            .file("/workspace/c.ncl", "{ answer = 42 }")
            .build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let transitive: TransitiveSource = resolve_transitive_source(
            "import \"a.ncl\"",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        let paths: Vec<PathBuf> = transitive
            .files()
            .iter()
            .map(|(file, _): &(RelativeFile, String)| -> PathBuf { file.as_ref().to_path_buf() })
            .collect();
        for expected in ["main.ncl", "a.ncl", "b.ncl", "c.ncl"] {
            assert!(
                paths.contains(&PathBuf::from(expected)),
                "missing {expected}, paths were: {paths:?}"
            );
        }
    }

    #[test]
    fn a_diamond_import_is_read_once_and_resolved_from_cache_the_second_time() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.ncl", "import \"shared.ncl\"")
            .file("/workspace/b.ncl", "import \"shared.ncl\"")
            .file("/workspace/shared.ncl", "{ answer = 42 }")
            .build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let transitive: TransitiveSource = resolve_transitive_source(
            "{ a = import \"a.ncl\", b = import \"b.ncl\" }",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        )
        .unwrap();
        let shared_entries: usize = transitive
            .files()
            .iter()
            .filter(|(file, _): &&(RelativeFile, String)| file.as_ref() == Path::new("shared.ncl"))
            .count();
        assert_eq!(shared_entries, 1);
    }

    #[test]
    fn a_malformed_import_is_a_build_definition_error() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/broken.ncl", "{ unterminated =")
            .build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let result: SindriResult<TransitiveSource> = resolve_transitive_source(
            "import \"broken.ncl\"",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn a_malformed_top_level_script_is_a_build_definition_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let result: SindriResult<TransitiveSource> = resolve_transitive_source(
            "{ unterminated =",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn a_package_import_is_a_build_definition_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let result: SindriResult<TransitiveSource> = resolve_transitive_source(
            "import some_package",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        );
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        assert!(error.to_string().contains("package"), "message was: {error}");
    }

    #[test]
    fn files_and_get_path_delegate_to_the_wrapped_cache_hub() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let root: WorkspaceRoot = workspace_root();
        let mut hub: CacheHub = CacheHub::new();
        let mut pos_table: PosTable = PosTable::new();
        let main_source_path: SourcePath = SourcePath::Path(source_path().as_ref().to_path_buf(), InputFormat::Nickel);
        let main_file_id: FileId = hub.sources.add_string(main_source_path, "{}".to_string());
        hub.parse_to_term(&mut pos_table, main_file_id, InputFormat::Nickel)
            .unwrap();
        let mut file_contents: HashMap<AbsoluteFile, NickelSource> = HashMap::new();
        let resolver: WorkspaceImportResolver<'_, _> = WorkspaceImportResolver {
            hub: &mut hub,
            workspace_root: &root,
            file_system: &runtime,
            file_contents: &mut file_contents,
        };
        assert_eq!(resolver.files().source(main_file_id), "{}");
        assert_eq!(
            resolver.get_path(main_file_id).unwrap().to_string_lossy(),
            "/workspace/main.ncl"
        );
        assert!(resolver.get(main_file_id).is_some());
    }

    #[test]
    fn an_escaping_import_is_a_build_definition_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/outside.ncl", "{}").build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let result: SindriResult<TransitiveSource> = resolve_transitive_source(
            "import \"../outside.ncl\"",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        );
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        assert!(error.to_string().contains("workspace root"), "message was: {error}");
    }

    #[test]
    fn evaluation_reads_no_file_sindri_did_not_seed() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let result: SindriResult<TransitiveSource> = resolve_transitive_source(
            "import \"helper.ncl\"",
            &source_path(),
            &workspace_root(),
            &mut resolution_state,
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn an_escaping_import_is_rejected_even_when_the_target_exists_on_real_disk() {
        let temporary_directory: TempDir = TempDir::new().unwrap();
        let root_path: PathBuf = temporary_directory.path().to_path_buf();
        create_dir_all(root_path.join("workspace")).unwrap();
        write(root_path.join("workspace/main.ncl"), "import \"../secret.ncl\"").unwrap();
        write(root_path.join("secret.ncl"), "{ leaked = true }").unwrap();
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(root_path.join("workspace")));
        let source: String = std::fs::read_to_string(root_path.join("workspace/main.ncl")).unwrap();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let result: SindriResult<TransitiveSource> = resolve_transitive_source(
            &source,
            &AbsoluteFile::new(root_path.join("workspace/main.ncl")),
            &root,
            &mut resolution_state,
            &SystemFileSystem,
        );
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        assert!(error.to_string().contains("workspace root"), "message was: {error}");
    }
}
