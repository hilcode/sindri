use crate::error::SindriError;
use crate::error::SindriResult;
use crate::file_set::FileSet;
use crate::file_set::FileSetPattern;
use crate::nickel_import::TransitiveSource;
use crate::nickel_import::resolve_transitive_source;
use crate::parameter::ParameterBinding;
use crate::parameter::ParameterDeclarations;
use crate::parameter::ParameterState;
use crate::runtime::FileSystem;
use crate::script::Command;
use crate::script::Script;
use crate::script::ScriptInputs;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::RelativeDirectory;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use blake3::Hasher;
use serde::Deserialize;
use serde::Serialize;
use smol_str::SmolStr;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;
use std::io::Error as IoError;

/// The version of this crate, folded into every [`DefinitionHash`] alongside [`NICKEL_VERSION`]: a
/// bump of either dirties every task, a deliberate over-approximation since both shape how a
/// script's expression evaluates.
const SINDRI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The version of `nickel-lang-core` this crate evaluates scripts with, read out of `Cargo.lock` by
/// `build.rs` — so it always matches the pinned dependency exactly and can never drift the way a
/// hand-maintained constant could.
const NICKEL_VERSION: &str = env!("NICKEL_LANG_CORE_VERSION");

/// A field delimiter folded into the definition hash between fields, so that two different
/// splittings of the same byte stream can never collide. Without it, hashing the fields `["ab",
/// "c"]` and `["a", "bc"]` would concatenate to the identical bytes `abc` and produce the same
/// hash, even though they are different inputs.
const FIELD_SEPARATOR: [u8; 1] = [0];

/// The name of a [`Task`], unique within its owning module.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TaskName(SmolStr);

impl TaskName {
    pub fn new(name: impl Into<SmolStr>) -> TaskName {
        TaskName(name.into())
    }
}

impl Display for TaskName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

/// The `TaskInput` a task's build file authors — "where are my sources." Distinct from
/// [`ManagedTaskInput`] so the two can never be swapped positionally when constructing a [`Task`].
#[derive(Clone, Debug)]
pub struct DeclaredTaskInput(FileSetPattern);

impl DeclaredTaskInput {
    pub fn new(pattern: FileSetPattern) -> DeclaredTaskInput {
        DeclaredTaskInput(pattern)
    }

    pub fn pattern(&self) -> &FileSetPattern {
        &self.0
    }
}

/// The `TaskInput` a plugin contributes for artifacts it generated and already knows the location
/// of (e.g. a generated `go.work`). Distinct from [`DeclaredTaskInput`] for the same reason.
#[derive(Clone, Debug)]
pub struct ManagedTaskInput(FileSetPattern);

impl ManagedTaskInput {
    pub fn new(pattern: FileSetPattern) -> ManagedTaskInput {
        ManagedTaskInput(pattern)
    }

    pub fn pattern(&self) -> &FileSetPattern {
        &self.0
    }
}

/// The `FileSetPattern` naming the files a [`Task`] produces, resolved against the task's output
/// directory after its script runs.
#[derive(Clone, Debug)]
pub struct TaskOutput(FileSetPattern);

impl TaskOutput {
    pub fn new(pattern: FileSetPattern) -> TaskOutput {
        TaskOutput(pattern)
    }

    pub fn pattern(&self) -> &FileSetPattern {
        &self.0
    }
}

/// A `Script`, its declared and managed inputs, its output, and its declared parameters — the unit
/// Sindri schedules. A `Task` is a description: it holds no file lists, no parameter values, and no
/// concrete commands. [`Task::resolve`] binds it for a concrete build.
#[derive(Clone, Debug)]
pub struct Task {
    name: TaskName,
    script: Script,
    declared_input: DeclaredTaskInput,
    managed_input: ManagedTaskInput,
    output: TaskOutput,
    declared_parameters: ParameterDeclarations,
}

impl Task {
    pub fn new(
        name: TaskName,
        script: Script,
        declared_input: DeclaredTaskInput,
        managed_input: ManagedTaskInput,
        output: TaskOutput,
        declared_parameters: ParameterDeclarations,
    ) -> Task {
        Task {
            name,
            script,
            declared_input,
            managed_input,
            output,
            declared_parameters,
        }
    }

    pub fn name(&self) -> &TaskName {
        &self.name
    }

    pub fn script(&self) -> &Script {
        &self.script
    }

    pub fn declared_input(&self) -> &DeclaredTaskInput {
        &self.declared_input
    }

    pub fn managed_input(&self) -> &ManagedTaskInput {
        &self.managed_input
    }

    pub fn output(&self) -> &TaskOutput {
        &self.output
    }

    pub fn declared_parameters(&self) -> &ParameterDeclarations {
        &self.declared_parameters
    }

    /// Resolve this task for a concrete build: bind `parameter_state` against the task's declared
    /// parameters, match its declared input against the module directory and its managed input
    /// against `managed_input_base` — their union is the task's effective input — and apply the
    /// script to the bound parameters and effective input to yield the ordered commands to run.
    /// `managed_input_base` is distinct from `module_directory` because a managed input need not live
    /// in the module at all: `go-compile`'s managed `go.work`, for instance, lives in the
    /// `generate-go-work` task's own output directory, shared workspace-wide.
    pub fn resolve(
        &self,
        parameter_state: &ParameterState,
        module_directory: &RelativeDirectory,
        managed_input_base: &AbsoluteDirectory,
        output_directory: &AbsoluteDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> SindriResult<Vec<Command>> {
        let binding: ParameterBinding = ParameterBinding::resolve(&self.declared_parameters, parameter_state)?;
        let module_directory_absolute: AbsoluteDirectory =
            workspace_root.to_absolute_directory().join_directory(module_directory);
        let declared_files: FileSet = resolve_file_set(
            self.declared_input.pattern(),
            &module_directory_absolute,
            workspace_root,
            file_system,
        )?;
        let managed_files: FileSet = resolve_file_set(
            self.managed_input.pattern(),
            managed_input_base,
            workspace_root,
            file_system,
        )?;
        let effective_input: FileSet = declared_files.union(&managed_files);
        let inputs: ScriptInputs = ScriptInputs::new(
            &binding,
            &effective_input,
            output_directory,
            managed_input_base,
            workspace_root,
            module_directory,
        );
        self.script
            .evaluate(&inputs, &self.script_path(&module_directory_absolute), file_system)
    }

    /// Where this task's script is addressed for the purpose of resolving its own relative
    /// imports — a synthetic location within the module directory, since the script itself is
    /// embedded Nickel source rather than a real file on disk. Shared by [`Task::resolve`] and
    /// [`Task::definition_hash`] so both anchor the same script's imports identically.
    fn script_path(&self, module_directory_absolute: &AbsoluteDirectory) -> AbsoluteFile {
        module_directory_absolute
            .join_file(&RelativeFile::new(format!("{}.ncl", self.name)).expect("a task name is always well-formed"))
    }

    /// This task's definition hash (see [`DefinitionHash`]), computed against the version of Sindri
    /// and `nickel-lang-core` this binary was built with. The script's own source is addressed as if
    /// it lived in `module_directory`, so its own relative imports resolve there; `workspace_root`
    /// bounds where those imports may resolve to, and `file_system` is the sole channel through
    /// which they — and the script itself — are read, so the computation stays hermetic under a test
    /// runtime.
    pub fn definition_hash(
        &self,
        module_directory: &RelativeDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> SindriResult<DefinitionHash> {
        self.definition_hash_with_salt(
            module_directory,
            workspace_root,
            file_system,
            SINDRI_VERSION,
            NICKEL_VERSION,
        )
    }

    /// The definition hash computation, with the version salt supplied explicitly rather than read
    /// from this build's own version constants — so a test can prove the salt actually participates
    /// in the hash by supplying two different ones and observing two different hashes.
    fn definition_hash_with_salt(
        &self,
        module_directory: &RelativeDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
        sindri_version: &str,
        nickel_version: &str,
    ) -> SindriResult<DefinitionHash> {
        let module_directory_absolute: AbsoluteDirectory =
            workspace_root.to_absolute_directory().join_directory(module_directory);
        let script_path: AbsoluteFile = self.script_path(&module_directory_absolute);
        let transitive_source: TransitiveSource =
            resolve_transitive_source(self.script.source(), &script_path, workspace_root, file_system)?;
        let mut hasher: Hasher = Hasher::new();
        update_field(&mut hasher, &self.name.to_string());
        for (path, content) in transitive_source.files() {
            update_field(&mut hasher, &path.to_string());
            update_field(&mut hasher, content);
        }
        for pattern_hash in [
            self.declared_input.pattern().pattern_hash(),
            self.managed_input.pattern().pattern_hash(),
            self.output.pattern().pattern_hash(),
        ] {
            hasher.update(pattern_hash.as_bytes());
            hasher.update(&FIELD_SEPARATOR);
        }
        for (plugin, name, parameter_type) in self.declared_parameters.iter() {
            update_field(&mut hasher, &plugin.to_string());
            update_field(&mut hasher, &name.to_string());
            update_field(&mut hasher, parameter_type.source());
        }
        update_field(&mut hasher, sindri_version);
        update_field(&mut hasher, nickel_version);
        Ok(DefinitionHash(*hasher.finalize().as_bytes()))
    }
}

/// A digest over everything that describes a [`Task`] independently of the current workspace
/// contents or the chosen parameter values: its name, its script's transitive Nickel source, its
/// input and output pattern hashes, its declared parameters, and a `(Sindri version, Nickel
/// version)` salt. When it changes, the task is stale by definition and must run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DefinitionHash([u8; 32]);

fn update_field(hasher: &mut Hasher, field: &str) {
    hasher.update(field.as_bytes());
    hasher.update(&FIELD_SEPARATOR);
}

/// Resolve `pattern` against `base`, converting the low-level filesystem error into a
/// [`SindriError`] so callers return one uniform error type.
pub(crate) fn resolve_file_set(
    pattern: &FileSetPattern,
    base: &AbsoluteDirectory,
    root: &WorkspaceRoot,
    file_system: &impl FileSystem,
) -> SindriResult<FileSet> {
    FileSet::resolve(pattern, base, root, file_system).map_err(|source: IoError| -> SindriError {
        SindriError::Io {
            path: base.as_ref().to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parameter::Parameter;
    use crate::parameter::ParameterName;
    use crate::parameter::ParameterType;
    use crate::parameter::ParameterValue;
    use crate::parameter::PluginName;
    use crate::runtime::DummyRuntime;
    use std::path::PathBuf;

    const WORKSPACE: &str = "/workspace";
    const MODULE: &str = "libs/common";

    fn module_directory() -> RelativeDirectory {
        RelativeDirectory::new_unchecked(MODULE)
    }

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)))
    }

    fn output_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"))
    }

    fn managed_input_base() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"))
    }

    #[test]
    fn a_task_exposes_its_name_script_inputs_output_and_declared_parameters() {
        let declared_input: DeclaredTaskInput = DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"]));
        let managed_input: ManagedTaskInput = ManagedTaskInput::new(FileSetPattern::new(["go.work"]));
        let output: TaskOutput = TaskOutput::new(FileSetPattern::new(["**/*"]));
        let declared_parameters: ParameterDeclarations = ParameterDeclarations::new([Parameter::new(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterType::new("String"),
        )]);
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => []"),
            declared_input.clone(),
            managed_input.clone(),
            output.clone(),
            declared_parameters.clone(),
        );
        assert_eq!(task.name().to_string(), "go-compile");
        assert_eq!(task.script().source(), "fun inputs => []");
        assert_eq!(
            task.declared_input().pattern().globs(),
            declared_input.pattern().globs()
        );
        assert_eq!(task.managed_input().pattern().globs(), managed_input.pattern().globs());
        assert_eq!(task.output().pattern().globs(), output.pattern().globs());
        assert!(!task.declared_parameters().is_empty());
    }

    #[test]
    fn resolution_surfaces_an_invalid_input_pattern_as_an_io_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"go\" } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(["["])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        );
        let result: SindriResult<Vec<Command>> = task.resolve(
            &ParameterState::default(),
            &module_directory(),
            &managed_input_base(),
            &output_directory(),
            &workspace_root(),
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn resolution_surfaces_an_invalid_managed_pattern_as_an_io_error() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"go\" } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(["["])),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        );
        let result: SindriResult<Vec<Command>> = task.resolve(
            &ParameterState::default(),
            &module_directory(),
            &managed_input_base(),
            &output_directory(),
            &workspace_root(),
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn a_file_matched_only_by_the_managed_pattern_is_in_the_effective_input() {
        // The managed file lives under `managed_input_base()`, not the module directory — proving
        // the managed pattern resolves against its own base, distinct from the declared input's.
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(format!("{WORKSPACE}/{MODULE}/main.go"), "")
            .file(
                managed_input_base()
                    .as_ref()
                    .join("go.work")
                    .to_string_lossy()
                    .into_owned(),
                "",
            )
            .build();
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"echo\", arguments = inputs.files } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
            ManagedTaskInput::new(FileSetPattern::new(["go.work"])),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        );
        let commands: Vec<Command> = task
            .resolve(
                &ParameterState::default(),
                &module_directory(),
                &managed_input_base(),
                &output_directory(),
                &workspace_root(),
                &runtime,
            )
            .unwrap();
        assert_eq!(
            commands[0].arguments(),
            &[
                SmolStr::new(".target/generate-go-work/binding/go.work"),
                SmolStr::new("libs/common/main.go"),
            ]
        );
    }

    #[test]
    fn resolution_yields_the_concrete_commands_for_a_given_binding() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new(
                "fun inputs => [ { program = \"go\", arguments = [ \"build\", inputs.params.\"sindri-go\".mode ] } ]",
            ),
            DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(["**/*"])),
            ParameterDeclarations::new([Parameter::new(
                PluginName::new("sindri-go"),
                ParameterName::new("mode"),
                ParameterType::new("String"),
            )]),
        );
        let values: ParameterState = ParameterState::new([(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterValue::new("\"release\""),
        )]);
        let commands: Vec<Command> = task
            .resolve(
                &values,
                &module_directory(),
                &managed_input_base(),
                &output_directory(),
                &workspace_root(),
                &runtime,
            )
            .unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program(), "go");
        assert_eq!(
            commands[0].arguments(),
            &[SmolStr::new("build"), SmolStr::new("release")]
        );
    }

    #[test]
    fn resolution_fails_before_the_script_runs_when_a_parameter_is_missing() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"go\" } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::new([Parameter::new(
                PluginName::new("sindri-go"),
                ParameterName::new("mode"),
                ParameterType::new("String"),
            )]),
        );
        let result: SindriResult<Vec<Command>> = task.resolve(
            &ParameterState::default(),
            &module_directory(),
            &managed_input_base(),
            &output_directory(),
            &workspace_root(),
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::ParameterMissing { .. })));
    }

    fn go_compile_task() -> Task {
        Task::new(
            TaskName::new("go-compile"),
            Script::new("let helper = import \"helper.ncl\" in fun inputs => [ { program = helper.program } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(["**/*"])),
            ParameterDeclarations::new([Parameter::new(
                PluginName::new("sindri-go"),
                ParameterName::new("mode"),
                ParameterType::new("String"),
            )]),
        )
    }

    fn runtime_with_helper(helper_program: &str) -> DummyRuntime {
        DummyRuntime::builder()
            .file(
                format!("{WORKSPACE}/{MODULE}/helper.ncl"),
                format!("{{ program = \"{helper_program}\" }}"),
            )
            .build()
    }

    #[test]
    fn resolution_supports_a_script_that_imports_a_workspace_local_helper() {
        let task: Task = go_compile_task();
        let runtime: DummyRuntime = runtime_with_helper("go");
        let values: ParameterState = ParameterState::new([(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterValue::new("\"release\""),
        )]);
        let commands: Vec<Command> = task
            .resolve(
                &values,
                &module_directory(),
                &managed_input_base(),
                &output_directory(),
                &workspace_root(),
                &runtime,
            )
            .unwrap();
        assert_eq!(commands[0].program(), "go");
    }

    #[test]
    fn editing_an_imported_script_source_changes_the_definition_hash() {
        let task: Task = go_compile_task();
        let before: DefinitionHash = task
            .definition_hash(&module_directory(), &workspace_root(), &runtime_with_helper("go"))
            .unwrap();
        let after: DefinitionHash = task
            .definition_hash(
                &module_directory(),
                &workspace_root(),
                &runtime_with_helper("go-edited"),
            )
            .unwrap();
        assert_ne!(before, after);
    }

    #[test]
    fn the_same_task_and_source_reproduce_the_same_definition_hash() {
        let task: Task = go_compile_task();
        let first: DefinitionHash = task
            .definition_hash(&module_directory(), &workspace_root(), &runtime_with_helper("go"))
            .unwrap();
        let second: DefinitionHash = task
            .definition_hash(&module_directory(), &workspace_root(), &runtime_with_helper("go"))
            .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn a_version_salt_bump_changes_every_definition_hash() {
        let task: Task = go_compile_task();
        let runtime: DummyRuntime = runtime_with_helper("go");
        let before: DefinitionHash = task
            .definition_hash_with_salt(&module_directory(), &workspace_root(), &runtime, "0.1.0", "0.17.0")
            .unwrap();
        let sindri_bumped: DefinitionHash = task
            .definition_hash_with_salt(&module_directory(), &workspace_root(), &runtime, "0.2.0", "0.17.0")
            .unwrap();
        let nickel_bumped: DefinitionHash = task
            .definition_hash_with_salt(&module_directory(), &workspace_root(), &runtime, "0.1.0", "0.18.0")
            .unwrap();
        assert_ne!(before, sindri_bumped);
        assert_ne!(before, nickel_bumped);
    }

    #[test]
    fn changing_a_resolved_argument_or_environment_value_does_not_change_the_definition_hash() {
        // `Task::resolve` applies the script via `Script::evaluate`, which does not support
        // imports, so this task's script is self-contained rather than reusing
        // `go_compile_task`'s import-based one.
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new(
                "fun inputs => [ { program = \"go\", arguments = [ \"build\", inputs.params.\"sindri-go\".mode ] } ]",
            ),
            DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(["**/*"])),
            ParameterDeclarations::new([Parameter::new(
                PluginName::new("sindri-go"),
                ParameterName::new("mode"),
                ParameterType::new("String"),
            )]),
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let before: DefinitionHash = task
            .definition_hash(&module_directory(), &workspace_root(), &runtime)
            .unwrap();
        for mode in ["debug", "release"] {
            let values: ParameterState = ParameterState::new([(
                PluginName::new("sindri-go"),
                ParameterName::new("mode"),
                ParameterValue::new(format!("\"{mode}\"")),
            )]);
            let commands: Vec<Command> = task
                .resolve(
                    &values,
                    &module_directory(),
                    &managed_input_base(),
                    &output_directory(),
                    &workspace_root(),
                    &runtime,
                )
                .unwrap();
            assert_eq!(commands[0].program(), "go");
        }
        let after: DefinitionHash = task
            .definition_hash(&module_directory(), &workspace_root(), &runtime)
            .unwrap();
        assert_eq!(before, after);
    }
}
