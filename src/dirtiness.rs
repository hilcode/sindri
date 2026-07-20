use crate::error::SindriError;
use crate::error::SindriResult;
use crate::file_set::ContentHash;
use crate::file_set::FileSet;
use crate::metadata_cache::MetadataCache;
use crate::parameter::BindingHash;
use crate::runtime::FileSystem;
use crate::task::DefinitionHash;
use crate::task::Task;
use crate::task::TaskName;
use crate::task::resolve_file_set;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::RelativeDirectory;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use serde::Deserialize;
use serde::Serialize;
use std::io::Error as IoError;
use std::io::Result as IoResult;

/// Where a single resolved task — a `(Task, BindingHash)` pair — owns its state on disk, rooted at
/// `<build-directory>/<module-directory>/<task-name>/`. [`TaskLayout::output_directory`] is the
/// binding's own `<binding-hash>/` subdirectory, so two bindings of the same task coexist side by
/// side without overwriting each other. [`TaskLayout::run_record_file`] is a sibling
/// `<binding-hash>.bin`, kept outside that directory so a `TaskOutput` pattern such as `**/*` can
/// never match Sindri's own bookkeeping as one of the task's artifacts.
pub struct TaskLayout {
    output_directory: AbsoluteDirectory,
    run_record_file: AbsoluteFile,
}

impl TaskLayout {
    pub fn new(
        build_directory: &AbsoluteDirectory,
        module_directory: &RelativeDirectory,
        task_name: &TaskName,
        binding_hash: BindingHash,
    ) -> TaskLayout {
        let task_directory: AbsoluteDirectory = build_directory
            .join_directory(module_directory)
            .join_directory(&RelativeDirectory::new(task_name.to_string()));
        let hexadecimal: String = binding_hash.to_hex();
        TaskLayout {
            output_directory: task_directory.join_directory(&RelativeDirectory::new(hexadecimal.clone())),
            run_record_file: task_directory.join_file(&RelativeFile::new(format!("{hexadecimal}.bin"))),
        }
    }

    pub fn output_directory(&self) -> &AbsoluteDirectory {
        &self.output_directory
    }

    pub fn run_record_file(&self) -> &AbsoluteFile {
        &self.run_record_file
    }
}

/// A resolved task's persisted fingerprint: its definition hash and the content hash of its input
/// and output file sets, exactly as they stood the last time it ran. The next build re-derives the
/// same three values with [`TaskRunRecord::compute`] and compares; any difference means the task
/// must run again — see [`dirtiness`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRunRecord {
    definition_hash: DefinitionHash,
    input_content_hash: ContentHash,
    output_content_hash: ContentHash,
}

impl TaskRunRecord {
    pub fn compute(
        task: &Task,
        module_directory: &RelativeDirectory,
        managed_input_base: &AbsoluteDirectory,
        output_directory: &AbsoluteDirectory,
        workspace_root: &WorkspaceRoot,
        cache: &MetadataCache,
        file_system: &impl FileSystem,
    ) -> SindriResult<TaskRunRecord> {
        let definition_hash: DefinitionHash = task.definition_hash(module_directory, workspace_root, file_system)?;
        let module_directory_absolute: AbsoluteDirectory =
            workspace_root.to_absolute_directory().join_directory(module_directory);
        let declared_files: FileSet = resolve_file_set(
            task.declared_input().pattern(),
            &module_directory_absolute,
            workspace_root,
            file_system,
        )?;
        let managed_files: FileSet = resolve_file_set(
            task.managed_input().pattern(),
            managed_input_base,
            workspace_root,
            file_system,
        )?;
        let output_files: FileSet =
            resolve_file_set(task.output().pattern(), output_directory, workspace_root, file_system)?;
        Ok(TaskRunRecord {
            definition_hash,
            input_content_hash: content_hash(
                &declared_files.union(&managed_files),
                cache,
                workspace_root,
                file_system,
            )?,
            output_content_hash: content_hash(&output_files, cache, workspace_root, file_system)?,
        })
    }

    pub fn load(path: &AbsoluteFile, file_system: &impl FileSystem) -> Option<TaskRunRecord> {
        let bytes: Vec<u8> = file_system.read(path.as_ref()).ok()?;
        rmp_serde::from_slice(&bytes).ok()
    }

    pub fn persist(&self, path: &AbsoluteFile, file_system: &impl FileSystem) -> IoResult<()> {
        let bytes: Vec<u8> = rmp_serde::to_vec(self).map_err(IoError::other)?;
        if let Some(parent) = path.parent() {
            file_system.create_directories(parent.as_ref())?;
        }
        file_system.write(path.as_ref(), &bytes)
    }
}

fn content_hash(
    file_set: &FileSet,
    cache: &MetadataCache,
    workspace_root: &WorkspaceRoot,
    file_system: &impl FileSystem,
) -> SindriResult<ContentHash> {
    cache
        .content_hash(file_set, workspace_root, file_system)
        .map_err(|source: IoError| -> SindriError {
            SindriError::Io {
                path: workspace_root.as_ref().to_path_buf(),
                source,
            }
        })
}

/// Whether a resolved task must run again: [`Dirtiness::Dirty`] when its persisted [`TaskRunRecord`]
/// is absent or differs from its freshly computed one, [`Dirtiness::Clean`] otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dirtiness {
    Clean,
    Dirty,
}

impl Dirtiness {
    /// The Chrome-trace `args.cache` value for this status.
    pub fn label(&self) -> &'static str {
        match self {
            Dirtiness::Clean => "hit",
            Dirtiness::Dirty => "miss",
        }
    }
}

pub fn dirtiness(current: &TaskRunRecord, persisted: Option<&TaskRunRecord>) -> Dirtiness {
    match persisted {
        Some(previous) if previous == current => Dirtiness::Clean,
        _ => Dirtiness::Dirty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_set::FileSetPattern;
    use crate::parameter::Parameter;
    use crate::parameter::ParameterBinding;
    use crate::parameter::ParameterDeclarations;
    use crate::parameter::ParameterName;
    use crate::parameter::ParameterType;
    use crate::parameter::ParameterValue;
    use crate::parameter::ParameterValues;
    use crate::parameter::PluginName;
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::script::Script;
    use crate::task::DeclaredTaskInput;
    use crate::task::ManagedTaskInput;
    use crate::task::TaskOutput;
    use std::path::PathBuf;

    const WORKSPACE: &str = "/workspace";
    const MODULE: &str = "libs/common";

    fn module_directory() -> RelativeDirectory {
        RelativeDirectory::new(MODULE)
    }

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)))
    }

    fn build_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace/.target"))
    }

    fn metadata_cache() -> MetadataCache {
        MetadataCache::new(AbsoluteDirectory::new(PathBuf::from(
            "/workspace/.target/.metadata-cache",
        )))
    }

    fn managed_input_base() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"))
    }

    fn go_compile_task(script_source: &str) -> Task {
        Task::new(
            TaskName::new("go-compile"),
            Script::new(script_source),
            DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(["**/*"])),
            ParameterDeclarations::default(),
        )
    }

    fn binding_hash(mode: &str) -> BindingHash {
        let declared: ParameterDeclarations = ParameterDeclarations::new([Parameter::new(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterType::new("String"),
        )]);
        let values: ParameterValues = ParameterValues::new([(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterValue::new(format!("\"{mode}\"")),
        )]);
        ParameterBinding::resolve(&declared, &values).unwrap().binding_hash()
    }

    fn layout(binding_hash: BindingHash) -> TaskLayout {
        TaskLayout::new(
            &build_directory(),
            &module_directory(),
            &TaskName::new("go-compile"),
            binding_hash,
        )
    }

    fn record(task: &Task, runtime: &DummyRuntime, output_directory: &AbsoluteDirectory) -> TaskRunRecord {
        TaskRunRecord::compute(
            task,
            &module_directory(),
            &managed_input_base(),
            output_directory,
            &workspace_root(),
            &metadata_cache(),
            runtime,
        )
        .unwrap()
    }

    fn workspace_with(source: &str) -> DummyRuntime {
        DummyRuntime::builder()
            .file(format!("{WORKSPACE}/{MODULE}/main.go"), source)
            .build()
    }

    #[test]
    fn two_bindings_of_one_task_produce_two_directories_side_by_side() {
        let debug: TaskLayout = layout(binding_hash("debug"));
        let release: TaskLayout = layout(binding_hash("release"));
        assert_ne!(debug.output_directory(), release.output_directory());
        assert_ne!(debug.run_record_file().as_ref(), release.run_record_file().as_ref());
        assert!(
            debug
                .output_directory()
                .as_ref()
                .starts_with("/workspace/.target/libs/common/go-compile")
        );
    }

    #[test]
    fn an_invalid_declared_input_pattern_surfaces_as_an_io_error() {
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"go\" } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(["["])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        );
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let result: SindriResult<TaskRunRecord> = TaskRunRecord::compute(
            &task,
            &module_directory(),
            &managed_input_base(),
            &output_directory,
            &workspace_root(),
            &metadata_cache(),
            &workspace_with("package common"),
        );
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn an_invalid_managed_input_pattern_surfaces_as_an_io_error() {
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"go\" } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(["["])),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        );
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let result: SindriResult<TaskRunRecord> = TaskRunRecord::compute(
            &task,
            &module_directory(),
            &managed_input_base(),
            &output_directory,
            &workspace_root(),
            &metadata_cache(),
            &workspace_with("package common"),
        );
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn an_invalid_output_pattern_surfaces_as_an_io_error() {
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"go\" } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(["["])),
            ParameterDeclarations::default(),
        );
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let result: SindriResult<TaskRunRecord> = TaskRunRecord::compute(
            &task,
            &module_directory(),
            &managed_input_base(),
            &output_directory,
            &workspace_root(),
            &metadata_cache(),
            &workspace_with("package common"),
        );
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn load_returns_none_for_a_missing_run_record() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let missing: AbsoluteFile = layout(binding_hash("debug")).run_record_file().clone();
        assert!(TaskRunRecord::load(&missing, &runtime).is_none());
    }

    #[test]
    fn persist_writes_directly_when_the_record_path_has_no_parent() {
        // `AbsoluteFile::parent()` is `None` only at the filesystem root — persisting there should
        // still write the record, just without first creating a (nonexistent) parent directory.
        let task: Task = go_compile_task("fun inputs => [ { program = \"go\" } ]");
        let runtime: DummyRuntime = workspace_with("package common");
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let record: TaskRunRecord = record(&task, &runtime, &output_directory);
        let root: AbsoluteFile = AbsoluteFile::new(PathBuf::from("/"));
        record.persist(&root, &runtime).unwrap();
        assert!(runtime.written_file(&root).is_some());
        assert!(!runtime.created_directory("/"));
    }

    #[test]
    fn switching_back_to_a_previously_built_binding_is_clean() {
        const SCRIPT: &str = "fun inputs => [ { program = \"go\", arguments = [ \"build\" ] } ]";
        let task: Task = go_compile_task(SCRIPT);
        let debug: TaskLayout = layout(binding_hash("debug"));
        let release: TaskLayout = layout(binding_hash("release"));
        let first_build: DummyRuntime = workspace_with("package common");
        let debug_record: TaskRunRecord = record(&task, &first_build, debug.output_directory());
        debug_record.persist(debug.run_record_file(), &first_build).unwrap();
        let release_record: TaskRunRecord = record(&task, &first_build, release.output_directory());
        release_record.persist(release.run_record_file(), &first_build).unwrap();
        let mut replay: DummyRuntimeBuilder =
            DummyRuntime::builder().file(format!("{WORKSPACE}/{MODULE}/main.go"), "package common");
        for (path, bytes) in first_build.written_files() {
            replay = replay.file(path, bytes);
        }
        let replay: DummyRuntime = replay.build();
        let persisted: Option<TaskRunRecord> = TaskRunRecord::load(debug.run_record_file(), &replay);
        let current: TaskRunRecord = record(&task, &replay, debug.output_directory());
        assert_eq!(dirtiness(&current, persisted.as_ref()), Dirtiness::Clean);
    }

    #[test]
    fn an_unchanged_task_is_clean() {
        const SCRIPT: &str = "fun inputs => [ { program = \"go\", arguments = [ \"build\" ] } ]";
        let task: Task = go_compile_task(SCRIPT);
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let runtime: DummyRuntime = workspace_with("package common");
        let baseline: TaskRunRecord = record(&task, &runtime, &output_directory);
        let current: TaskRunRecord = record(&task, &runtime, &output_directory);
        assert_eq!(dirtiness(&current, Some(&baseline)), Dirtiness::Clean);
    }

    #[test]
    fn two_tasks_sharing_an_input_file_read_it_only_once() {
        const SCRIPT: &str = "fun inputs => [ { program = \"go\", arguments = [ \"build\" ] } ]";
        let runtime: DummyRuntime = workspace_with("package common");
        let cache: MetadataCache = metadata_cache();
        let go_compile: Task = go_compile_task(SCRIPT);
        let go_test: Task = Task::new(
            TaskName::new("go-test"),
            Script::new(SCRIPT),
            DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        );
        let compile_output: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        TaskRunRecord::compute(
            &go_compile,
            &module_directory(),
            &managed_input_base(),
            &compile_output,
            &workspace_root(),
            &cache,
            &runtime,
        )
        .unwrap();
        TaskRunRecord::compute(
            &go_test,
            &module_directory(),
            &managed_input_base(),
            &compile_output,
            &workspace_root(),
            &cache,
            &runtime,
        )
        .unwrap();
        assert_eq!(runtime.read_count(format!("{WORKSPACE}/{MODULE}/main.go")), 1);
    }

    #[test]
    fn a_changed_definition_is_dirty() {
        const SCRIPT: &str = "fun inputs => [ { program = \"go\", arguments = [ \"build\" ] } ]";
        const EDITED_SCRIPT: &str = "fun inputs => [ { program = \"go\", arguments = [ \"vet\" ] } ]";
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let runtime: DummyRuntime = workspace_with("package common");
        let baseline: TaskRunRecord = record(&go_compile_task(SCRIPT), &runtime, &output_directory);
        let current: TaskRunRecord = record(&go_compile_task(EDITED_SCRIPT), &runtime, &output_directory);
        assert_eq!(dirtiness(&current, Some(&baseline)), Dirtiness::Dirty);
    }

    #[test]
    fn a_changed_input_file_is_dirty() {
        const SCRIPT: &str = "fun inputs => [ { program = \"go\", arguments = [ \"build\" ] } ]";
        let task: Task = go_compile_task(SCRIPT);
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let baseline: TaskRunRecord = record(&task, &workspace_with("package common"), &output_directory);
        let current: TaskRunRecord = record(&task, &workspace_with("package common // edited"), &output_directory);
        assert_eq!(dirtiness(&current, Some(&baseline)), Dirtiness::Dirty);
    }

    #[test]
    fn a_changed_output_file_is_dirty() {
        const SCRIPT: &str = "fun inputs => [ { program = \"go\", arguments = [ \"build\" ] } ]";
        let task: Task = go_compile_task(SCRIPT);
        let output_directory: AbsoluteDirectory = layout(binding_hash("debug")).output_directory().clone();
        let output_file: PathBuf = output_directory.as_ref().join("app");
        let before: DummyRuntime = DummyRuntime::builder()
            .file(format!("{WORKSPACE}/{MODULE}/main.go"), "package common")
            .file(&output_file, "binary")
            .build();
        let baseline: TaskRunRecord = record(&task, &before, &output_directory);
        let after: DummyRuntime = DummyRuntime::builder()
            .file(format!("{WORKSPACE}/{MODULE}/main.go"), "package common")
            .file(&output_file, "tampered")
            .build();
        let current: TaskRunRecord = record(&task, &after, &output_directory);
        assert_eq!(dirtiness(&current, Some(&baseline)), Dirtiness::Dirty);
    }
}
