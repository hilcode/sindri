use crate::error::SindriError;
use crate::error::SindriResult;
use crate::file_set::FileSet;
use crate::nickel_eval::Nickel;
use crate::nickel_import::evaluate_hermetically;
use crate::parameter::ParameterBinding;
use crate::runtime::FileSystem;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::RelativeDirectory;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use nickel_lang_core::eval::value::NickelValue;
use serde::Deserialize;
use smol_str::SmolStr;
use std::collections::BTreeMap;

/// The Nickel contract every command a script yields is checked against. Applied by Sindri, it makes
/// `program` required, fills the `arguments`/`environment` defaults, and defaults `working-directory`
/// to the module directory Sindri supplies.
const COMMAND_CONTRACT: &str = include_str!("contracts/command.ncl");

/// A single runnable process invocation produced by a [`Script`]: a program, its argument vector, its
/// complete environment, and the directory it runs in (relative to the workspace root). Deserialized
/// from a Nickel command record after the [`COMMAND_CONTRACT`] has validated it and applied defaults,
/// so every field is present and well-typed by the time this exists.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Command {
    program: SmolStr,
    arguments: Vec<SmolStr>,
    environment: BTreeMap<SmolStr, SmolStr>,
    #[serde(rename = "working-directory")]
    working_directory: RelativeDirectory,
}

impl Command {
    pub fn program(&self) -> &str {
        &self.program
    }

    pub fn arguments(&self) -> &[SmolStr] {
        &self.arguments
    }

    pub fn environment(&self) -> &BTreeMap<SmolStr, SmolStr> {
        &self.environment
    }

    pub fn working_directory(&self) -> &RelativeDirectory {
        &self.working_directory
    }
}

/// The values Sindri supplies to a [`Script`] expression: the task's bound parameters, the resolved
/// input files, the task's output directory, the workspace root, and the module directory each
/// command's `working-directory` defaults to. Paths reach the script as strings — workspace-relative
/// for `files`, absolute for the two directories — since a script computes with them but never reads
/// them.
pub struct ScriptInputs<'inputs> {
    parameters: &'inputs ParameterBinding,
    input_files: &'inputs FileSet,
    output_directory: &'inputs AbsoluteDirectory,
    workspace_root: &'inputs WorkspaceRoot,
    module_directory: &'inputs RelativeDirectory,
}

impl<'inputs> ScriptInputs<'inputs> {
    pub fn new(
        parameters: &'inputs ParameterBinding,
        input_files: &'inputs FileSet,
        output_directory: &'inputs AbsoluteDirectory,
        workspace_root: &'inputs WorkspaceRoot,
        module_directory: &'inputs RelativeDirectory,
    ) -> ScriptInputs<'inputs> {
        ScriptInputs {
            parameters,
            input_files,
            output_directory,
            workspace_root,
            module_directory,
        }
    }

    /// The Nickel record literal the script is applied to.
    fn to_nickel_record(&self) -> String {
        let files: Vec<String> = self
            .input_files
            .files()
            .iter()
            .map(|file: &RelativeFile| -> String { Nickel::string_literal(&file.to_string()) })
            .collect();
        format!(
            "{{ params = {parameters}, files = [ {files} ], \"output-directory\" = {output_directory}, \"workspace-root\" = {workspace_root} }}",
            parameters = self.parameters.to_nickel_record(),
            files = files.join(", "),
            output_directory = Nickel::string_literal(&self.output_directory.as_ref().to_string_lossy()),
            workspace_root = Nickel::string_literal(&self.workspace_root.as_ref().to_string_lossy()),
        )
    }
}

/// A task's script: a Nickel expression that, applied to a [`ScriptInputs`] record, evaluates to the
/// ordered list of [`Command`]s to run. The expression is pure — it computes command data and performs
/// no I/O — so Sindri owns both supplying the inputs and running the commands.
#[derive(Clone, Debug)]
pub struct Script {
    source: SmolStr,
}

impl Script {
    pub fn new(source: impl Into<SmolStr>) -> Script {
        Script { source: source.into() }
    }

    pub fn go_format() -> Script {
        Script::new(include_str!("scripts/go-format.ncl"))
    }

    pub fn go_compile() -> Script {
        Script::new(include_str!("scripts/go-compile.ncl"))
    }

    pub fn go_compile_executable() -> Script {
        Script::new(include_str!("scripts/go-compile-executable.ncl"))
    }

    pub fn go_test() -> Script {
        Script::new(include_str!("scripts/go-test.ncl"))
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// Apply the script to its inputs and return the commands it yields. The script is applied to the
    /// inputs record, each result element is checked against the command contract (which supplies the
    /// module directory as the `working-directory` default), and the list is deserialized. A script
    /// that yields a non-list, a non-record element, or a command missing its `program` fails here as
    /// a build-definition error.
    ///
    /// Evaluation is hermetic: the script is addressed as if it lived at `script_path`, so its own
    /// `import` statements resolve relative to that location, through `file_system` and bounded to
    /// `inputs`'s workspace root — never the real disk. This is the same mechanism
    /// `Task::definition_hash` uses to discover a script's transitive source, so a script's imports
    /// behave identically whether Sindri is hashing it or running it.
    pub fn evaluate(
        &self,
        inputs: &ScriptInputs,
        script_path: &AbsoluteFile,
        file_system: &impl FileSystem,
    ) -> SindriResult<Vec<Command>> {
        let source: String = format!(
            "let Command = ({command_contract}) {module_directory} in\n\
             let script = ({script}) in\n\
             std.array.map (fun command => command | Command) (script {inputs})",
            command_contract = COMMAND_CONTRACT,
            module_directory = Nickel::string_literal(&inputs.module_directory.as_ref().to_string_lossy()),
            script = self.source,
            inputs = inputs.to_nickel_record(),
        );
        let value: NickelValue = evaluate_hermetically(&source, script_path, inputs.workspace_root, file_system)?;
        Vec::<Command>::deserialize(value).map_err(|error| -> SindriError {
            SindriError::ScriptEvaluation {
                nickel_message: error.to_string(),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_set::FileSetPattern;
    use crate::parameter::Parameter;
    use crate::parameter::ParameterDeclarations;
    use crate::parameter::ParameterName;
    use crate::parameter::ParameterType;
    use crate::parameter::ParameterValue;
    use crate::parameter::ParameterValues;
    use crate::parameter::PluginName;
    use crate::runtime::DummyRuntime;
    use std::path::PathBuf;

    const WORKSPACE: &str = "/workspace";
    const MODULE: &str = "libs/common";

    fn file_set(files: &[&str]) -> FileSet {
        let mut builder = DummyRuntime::builder();
        for file in files {
            builder = builder.file(format!("{WORKSPACE}/{file}"), "");
        }
        let runtime: DummyRuntime = builder.build();
        let root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        FileSet::resolve(
            &FileSetPattern::new(["**/*"]),
            &root.to_absolute_directory(),
            &root,
            &runtime,
        )
        .unwrap()
    }

    fn script_path() -> AbsoluteFile {
        AbsoluteFile::new(PathBuf::from(format!("{WORKSPACE}/{MODULE}/script.ncl")))
    }

    fn evaluate(script_source: &str, files: &[&str]) -> SindriResult<Vec<Command>> {
        let parameters: ParameterBinding = ParameterBinding::empty();
        let input_files: FileSet = file_set(files);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &workspace_root,
            &module_directory,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        Script::new(script_source).evaluate(&inputs, &script_path(), &runtime)
    }

    #[test]
    fn a_script_exposes_its_source() {
        assert_eq!(Script::new("fun inputs => []").source(), "fun inputs => []");
    }

    #[test]
    fn a_command_missing_its_program_is_a_contract_error() {
        let result: SindriResult<Vec<Command>> = evaluate("fun inputs => [ { arguments = [ \"x\" ] } ]", &[]);
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        // The Nickel diagnostic names the missing field and points at the offending record.
        assert!(error.to_string().contains("program"), "message was: {error}");
    }

    #[test]
    fn a_script_yielding_two_command_records_yields_two_commands_in_order() {
        let commands: Vec<Command> = evaluate(
            "fun inputs => [ { program = \"first\" }, { program = \"second\" } ]",
            &[],
        )
        .unwrap();
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].program(), "first");
        assert_eq!(commands[1].program(), "second");
    }

    #[test]
    fn a_non_list_result_is_a_build_definition_error() {
        let result: SindriResult<Vec<Command>> = evaluate("fun inputs => { program = \"go\" }", &[]);
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn a_non_record_element_is_a_build_definition_error() {
        let result: SindriResult<Vec<Command>> = evaluate("fun inputs => [ 42 ]", &[]);
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn a_command_defaults_its_arguments_environment_and_working_directory() {
        let commands: Vec<Command> = evaluate("fun inputs => [ { program = \"go\" } ]", &[]).unwrap();
        let command: &Command = &commands[0];
        assert!(command.arguments().is_empty());
        assert!(command.environment().is_empty());
        // Absent `working-directory` defaults to the module directory Sindri supplies.
        assert_eq!(command.working_directory().as_ref(), PathBuf::from(MODULE));
    }

    #[test]
    fn a_script_can_read_the_supplied_input_files() {
        let commands: Vec<Command> = evaluate(
            "fun inputs => [ { program = \"echo\", arguments = inputs.files } ]",
            &["a.go", "b.go"],
        )
        .unwrap();
        assert_eq!(commands[0].arguments(), &[SmolStr::new("a.go"), SmolStr::new("b.go")]);
    }

    #[test]
    fn a_script_can_import_a_workspace_local_helper() {
        let parameters: ParameterBinding = ParameterBinding::empty();
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &workspace_root,
            &module_directory,
        );
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(format!("{WORKSPACE}/{MODULE}/helper.ncl"), "\"go\"")
            .build();
        let commands: Vec<Command> = Script::new("fun inputs => [ { program = import \"helper.ncl\" } ]")
            .evaluate(&inputs, &script_path(), &runtime)
            .unwrap();
        assert_eq!(commands[0].program(), "go");
    }

    #[test]
    fn a_script_can_read_a_bound_parameter() {
        let declared: ParameterDeclarations = ParameterDeclarations::new([Parameter::new(
            PluginName::new("plugin"),
            ParameterName::new("mode"),
            ParameterType::new("String"),
        )]);
        let values: ParameterValues = ParameterValues::new([(
            PluginName::new("plugin"),
            ParameterName::new("mode"),
            ParameterValue::new("\"debug\""),
        )]);
        let parameters: ParameterBinding = ParameterBinding::resolve(&declared, &values).unwrap();
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &workspace_root,
            &module_directory,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let commands: Vec<Command> =
            Script::new("fun inputs => [ { program = \"echo\", arguments = [ inputs.params.\"plugin\".mode ] } ]")
                .evaluate(&inputs, &script_path(), &runtime)
                .unwrap();
        assert_eq!(commands[0].arguments(), &[SmolStr::new("debug")]);
    }

    #[test]
    fn a_script_reading_an_undeclared_parameter_is_a_contract_error() {
        let declared: ParameterDeclarations = ParameterDeclarations::new([Parameter::new(
            PluginName::new("plugin"),
            ParameterName::new("mode"),
            ParameterType::new("String"),
        )]);
        let values: ParameterValues = ParameterValues::new([(
            PluginName::new("plugin"),
            ParameterName::new("mode"),
            ParameterValue::new("\"debug\""),
        )]);
        let parameters: ParameterBinding = ParameterBinding::resolve(&declared, &values).unwrap();
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &workspace_root,
            &module_directory,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let result: SindriResult<Vec<Command>> = Script::new(
            "fun inputs => [ { program = \"echo\", arguments = [ inputs.params.verbose ] } ]",
        )
        .evaluate(&inputs, &script_path(), &runtime);
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    #[test]
    fn the_go_scripts_yield_their_toolchain_commands() {
        for (script, program, first_argument) in [
            (Script::go_format(), "gofmt", "-l"),
            (Script::go_compile(), "go", "build"),
            (Script::go_compile_executable(), "go", "build"),
            (Script::go_test(), "go", "test"),
        ] {
            let parameters: ParameterBinding = ParameterBinding::empty();
            let input_files: FileSet = file_set(&[]);
            let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
            let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
            let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
            let inputs: ScriptInputs = ScriptInputs::new(
                &parameters,
                &input_files,
                &output_directory,
                &workspace_root,
                &module_directory,
            );
            let runtime: DummyRuntime = DummyRuntime::builder().build();
            let commands: Vec<Command> = script.evaluate(&inputs, &script_path(), &runtime).unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].program(), program);
            assert_eq!(commands[0].arguments()[0], SmolStr::new(first_argument));
        }
    }

    #[test]
    fn go_compile_executable_writes_its_binary_to_the_output_directory() {
        let parameters: ParameterBinding = ParameterBinding::empty();
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &workspace_root,
            &module_directory,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let commands: Vec<Command> = Script::go_compile_executable()
            .evaluate(&inputs, &script_path(), &runtime)
            .unwrap();
        assert_eq!(
            commands[0].arguments(),
            &[
                SmolStr::new("build"),
                SmolStr::new("-o"),
                SmolStr::new("/workspace/.target/out/"),
                SmolStr::new("./...")
            ]
        );
    }
}
