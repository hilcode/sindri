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
use std::ffi::OsStr;
use std::io::sink;
use std::path::Path;
use std::path::PathBuf;

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
    file_system: &impl FileSystem,
) -> SindriResult<TransitiveSource> {
    let (hub, _main_file_id): (CacheHub, FileId) =
        resolve_hermetically(source, source_path, workspace_root, file_system)?;
    let mut files: Vec<(RelativeFile, String)> = hub
        .sources
        .file_paths
        .iter()
        .filter_map(
            |(file_id, source_path): (&FileId, &SourcePath)| -> Option<(RelativeFile, String)> {
                transitive_source_entry(*file_id, source_path, &hub, workspace_root)
            },
        )
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
    file_system: &impl FileSystem,
) -> SindriResult<NickelValue> {
    let (mut hub, main_file_id): (CacheHub, FileId) =
        resolve_hermetically(source, source_path, workspace_root, file_system)?;
    let mut pos_table: PosTable = PosTable::new();
    hub.prepare_stdlib(&mut pos_table)
        .map_err(|error: NickelError| -> SindriError { import_resolution_error(&mut hub.sources.files, error) })?;
    let main_value: NickelValue = hub
        .terms
        .get_owned(main_file_id)
        .expect("main_file_id was resolved by resolve_hermetically");
    // `eval_full` evaluates whatever `pending_contracts` a term already carries rather than deriving
    // them itself; this is the transform (`gen_pending_contracts`) that `nickel_lang::Context::eval_deep`
    // runs internally via `prepare_eval`. Skipping it here would turn every field contract into a no-op.
    let main_value: NickelValue = nickel_lang_core::transform::transform(&mut pos_table, main_value, None)
        .map_err(|error| -> SindriError { transform_error(&mut hub.sources.files, error) })?;
    let mut vm_context: VmContext<CacheHub, CacheImpl> =
        VmContext::new_with_pos_table(hub, pos_table, sink(), NullReporter {});
    let mut vm: VirtualMachine<'_, CacheHub, CacheImpl> = VirtualMachine::new(&mut vm_context);
    let result: Result<NickelValue, nickel_lang_core::error::EvalError> = vm.eval_full(main_value);
    // `VirtualMachine` implements `Drop`, which extends its mutable borrow of `vm_context` to the end
    // of scope regardless of last use — dropping it explicitly frees `vm_context` up for the error
    // path below to read `import_resolver.sources.files` back out.
    drop(vm);
    result.map_err(|error| -> SindriError {
        import_resolution_error(&mut vm_context.import_resolver.sources.files, error)
    })
}

/// Hermetically resolve `source`'s entire transitive import closure into a fresh [`CacheHub`],
/// addressed as if it lived at `source_path`. Every reached file's cached term has its imports
/// already replaced by `ResolvedImport` markers, so the returned hub is ready either to have its
/// `file_paths` read off directly (for the transitive source set) or to be evaluated as-is.
fn resolve_hermetically(
    source: &str,
    source_path: &AbsoluteFile,
    workspace_root: &WorkspaceRoot,
    file_system: &impl FileSystem,
) -> SindriResult<(CacheHub, FileId)> {
    let mut cache_hub: CacheHub = CacheHub::new();
    let mut pos_table: PosTable = PosTable::new();
    let main_source_path: SourcePath = SourcePath::Path(source_path.as_ref().to_path_buf(), InputFormat::Nickel);
    let main_file_id: FileId = cache_hub.sources.add_string(main_source_path, source.to_string());
    cache_hub
        .parse_to_term(&mut pos_table, main_file_id, InputFormat::Nickel)
        .map_err(|parse_errors: ParseErrors| -> SindriError {
            import_resolution_error(&mut cache_hub.sources.files, parse_errors)
        })?;
    let mut resolver: WorkspaceImportResolver<'_, _> = WorkspaceImportResolver {
        hub: cache_hub,
        workspace_root,
        file_system,
    };
    resolve_transitively(&mut pos_table, &mut resolver, main_file_id).map_err(
        |import_error: ImportError| -> SindriError {
            import_resolution_error(&mut resolver.hub.sources.files, import_error)
        },
    )?;
    Ok((resolver.hub, main_file_id))
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
/// hard-coded to `CacheHub` as its own resolver and cannot be reused with a different one; unlike
/// it, this only needs to write the transformed term back, not track intermediate resolution states,
/// since nothing here re-enters resolution for a file more than once.
fn resolve_transitively<FS: FileSystem>(
    pos_table: &mut PosTable,
    resolver: &mut WorkspaceImportResolver<'_, FS>,
    file_id: FileId,
) -> Result<(), ImportError> {
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
        resolve_transitively(pos_table, resolver, resolved_id)?;
    }
    Ok(())
}

/// An [`ImportResolver`] that resolves every import through Sindri's [`FileSystem`] instead of the
/// real disk, and rejects one resolving outside `workspace_root` before ever attempting to read it.
/// Wraps a [`CacheHub`] for source/term storage and delegates `files`/`get`/`get_path` to it
/// unchanged — only `resolve` differs from `CacheHub`'s own behaviour.
struct WorkspaceImportResolver<'runtime, FS: FileSystem> {
    hub: CacheHub,
    workspace_root: &'runtime WorkspaceRoot,
    file_system: &'runtime FS,
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
                let content: String = self.file_system.read_to_string(&normalized).map_err(
                    |io_error: std::io::Error| -> ImportError {
                        import_io_error(path.to_string_lossy().into_owned(), position, io_error)
                    },
                )?;
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
    fn evaluate_hermetically_evaluates_a_self_contained_script() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let value: NickelValue = evaluate_hermetically("21 + 21", &source_path(), &workspace_root(), &runtime).unwrap();
        let answer: i64 = i64::deserialize(value).unwrap();
        assert_eq!(answer, 42);
    }

    #[test]
    fn evaluate_hermetically_evaluates_an_imported_script() {
        let runtime: DummyRuntime = DummyRuntime::builder().file("/workspace/helper.ncl", "21").build();
        let value: NickelValue = evaluate_hermetically(
            "(import \"helper.ncl\") + 21",
            &source_path(),
            &workspace_root(),
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
        let transitive: TransitiveSource =
            resolve_transitive_source("import \"helper.ncl\"", &source_path(), &workspace_root(), &runtime).unwrap();
        let paths: Vec<PathBuf> = transitive
            .files()
            .iter()
            .map(|(file, _): &(RelativeFile, String)| -> PathBuf { file.as_ref().to_path_buf() })
            .collect();
        assert!(paths.contains(&PathBuf::from("main.ncl")), "paths were: {paths:?}");
        assert!(paths.contains(&PathBuf::from("helper.ncl")), "paths were: {paths:?}");
    }

    #[test]
    fn nested_imports_are_resolved_transitively() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/a.ncl", "import \"b.ncl\"")
            .file("/workspace/b.ncl", "import \"c.ncl\"")
            .file("/workspace/c.ncl", "{ answer = 42 }")
            .build();
        let transitive: TransitiveSource =
            resolve_transitive_source("import \"a.ncl\"", &source_path(), &workspace_root(), &runtime).unwrap();
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
        let transitive: TransitiveSource = resolve_transitive_source(
            "{ a = import \"a.ncl\", b = import \"b.ncl\" }",
            &source_path(),
            &workspace_root(),
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
        let result: SindriResult<TransitiveSource> =
            resolve_transitive_source("import \"broken.ncl\"", &source_path(), &workspace_root(), &runtime);
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn a_malformed_top_level_script_is_a_build_definition_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let result: SindriResult<TransitiveSource> =
            resolve_transitive_source("{ unterminated =", &source_path(), &workspace_root(), &runtime);
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn a_package_import_is_a_build_definition_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let result: SindriResult<TransitiveSource> =
            resolve_transitive_source("import some_package", &source_path(), &workspace_root(), &runtime);
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
        let resolver: WorkspaceImportResolver<'_, _> = WorkspaceImportResolver {
            hub,
            workspace_root: &root,
            file_system: &runtime,
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
        let result: SindriResult<TransitiveSource> =
            resolve_transitive_source("import \"../outside.ncl\"", &source_path(), &workspace_root(), &runtime);
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        assert!(error.to_string().contains("workspace root"), "message was: {error}");
    }

    #[test]
    fn evaluation_reads_no_file_sindri_did_not_seed() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let result: SindriResult<TransitiveSource> =
            resolve_transitive_source("import \"helper.ncl\"", &source_path(), &workspace_root(), &runtime);
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
        let result: SindriResult<TransitiveSource> = resolve_transitive_source(
            &source,
            &AbsoluteFile::new(root_path.join("workspace/main.ncl")),
            &root,
            &SystemFileSystem,
        );
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        assert!(error.to_string().contains("workspace root"), "message was: {error}");
    }
}
