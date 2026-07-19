use crate::error::SindriError;
use crate::error::SindriResult;
use crate::file_set::FileSet;
use crate::file_set::FileSetPattern;
use crate::parameter::ParameterBinding;
use crate::parameter::ParameterDeclarations;
use crate::parameter::ParameterValues;
use crate::runtime::FileSystem;
use crate::script::Command;
use crate::script::Script;
use crate::script::ScriptInputs;
use crate::types::AbsoluteDirectory;
use crate::types::RelativeDirectory;
use crate::types::WorkspaceRoot;
use smol_str::SmolStr;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;
use std::io::Error as IoError;

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

    /// Resolve this task for a concrete build: bind `parameter_values` against the task's declared
    /// parameters, match its declared and managed input patterns against the workspace — their union
    /// is the task's effective input — and apply the script to the bound parameters and effective
    /// input to yield the ordered commands to run.
    pub fn resolve(
        &self,
        parameter_values: &ParameterValues,
        module_directory: &RelativeDirectory,
        output_directory: &AbsoluteDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> SindriResult<Vec<Command>> {
        let binding: ParameterBinding = ParameterBinding::resolve(&self.declared_parameters, parameter_values)?;
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
            &module_directory_absolute,
            workspace_root,
            file_system,
        )?;
        let effective_input: FileSet = declared_files.union(&managed_files);
        let inputs: ScriptInputs = ScriptInputs::new(
            &binding,
            &effective_input,
            output_directory,
            workspace_root,
            module_directory,
        );
        self.script.evaluate(&inputs)
    }
}

/// Resolve `pattern` against `base`, converting the low-level filesystem error into a
/// [`SindriError`] so [`Task::resolve`] returns one uniform error type.
fn resolve_file_set(
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
        RelativeDirectory::new(MODULE)
    }

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)))
    }

    fn output_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"))
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
            &ParameterValues::default(),
            &module_directory(),
            &output_directory(),
            &workspace_root(),
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::Io { .. })));
    }

    #[test]
    fn a_file_matched_only_by_the_managed_pattern_is_in_the_effective_input() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(format!("{WORKSPACE}/{MODULE}/main.go"), "")
            .file(format!("{WORKSPACE}/{MODULE}/go.work"), "")
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
                &ParameterValues::default(),
                &module_directory(),
                &output_directory(),
                &workspace_root(),
                &runtime,
            )
            .unwrap();
        assert_eq!(
            commands[0].arguments(),
            &[SmolStr::new("libs/common/go.work"), SmolStr::new("libs/common/main.go")]
        );
    }

    #[test]
    fn resolution_yields_the_concrete_commands_for_a_given_binding() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task: Task = Task::new(
            TaskName::new("go-compile"),
            Script::new("fun inputs => [ { program = \"go\", arguments = [ \"build\", inputs.params.\"sindri-go\".mode ] } ]"),
            DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(["**/*"])),
            ParameterDeclarations::new([Parameter::new(
                PluginName::new("sindri-go"),
                ParameterName::new("mode"),
                ParameterType::new("String"),
            )]),
        );
        let values: ParameterValues = ParameterValues::new([(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterValue::new("\"release\""),
        )]);
        let commands: Vec<Command> = task
            .resolve(
                &values,
                &module_directory(),
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
            &ParameterValues::default(),
            &module_directory(),
            &output_directory(),
            &workspace_root(),
            &runtime,
        );
        assert!(matches!(result, Err(SindriError::ParameterMissing { .. })));
    }
}
