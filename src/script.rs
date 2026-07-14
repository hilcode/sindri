use crate::error::SindriError;
use crate::error::SindriResult;
use crate::file_set::FileSet;
use crate::nickel_eval::Nickel;
use crate::types::AbsoluteDirectory;
use crate::types::RelativeDirectory;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use nickel_lang::Expr;
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

/// The values Sindri supplies to a [`Script`] expression: the resolved input files, the task's output
/// directory, the workspace root, and the module directory each command's `working-directory` defaults
/// to. Paths reach the script as strings — workspace-relative for `files`, absolute for the two
/// directories — since a script computes with them but never reads them.
pub struct ScriptInputs<'inputs> {
    input_files: &'inputs FileSet,
    output_directory: &'inputs AbsoluteDirectory,
    workspace_root: &'inputs WorkspaceRoot,
    module_directory: &'inputs RelativeDirectory,
}

impl<'inputs> ScriptInputs<'inputs> {
    pub fn new(
        input_files: &'inputs FileSet,
        output_directory: &'inputs AbsoluteDirectory,
        workspace_root: &'inputs WorkspaceRoot,
        module_directory: &'inputs RelativeDirectory,
    ) -> ScriptInputs<'inputs> {
        ScriptInputs {
            input_files,
            output_directory,
            workspace_root,
            module_directory,
        }
    }

    /// The Nickel record literal the script is applied to. `params` is empty until plugin parameters
    /// land, but the field exists so the script's input shape does not change when they do.
    fn to_nickel_record(&self) -> String {
        let files: Vec<String> = self
            .input_files
            .files()
            .iter()
            .map(|file: &RelativeFile| -> String { nickel_string(&file.to_string()) })
            .collect();
        format!(
            "{{ params = {{}}, files = [ {files} ], \"output-directory\" = {output_directory}, \"workspace-root\" = {workspace_root} }}",
            files = files.join(", "),
            output_directory = nickel_string(&self.output_directory.as_ref().to_string_lossy()),
            workspace_root = nickel_string(&self.workspace_root.as_ref().to_string_lossy()),
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
    pub fn evaluate(&self, inputs: &ScriptInputs) -> SindriResult<Vec<Command>> {
        let source: String = format!(
            "let Command = ({command_contract}) {module_directory} in\n\
             let script = ({script}) in\n\
             std.array.map (fun command => command | Command) (script {inputs})",
            command_contract = COMMAND_CONTRACT,
            module_directory = nickel_string(&inputs.module_directory.as_ref().to_string_lossy()),
            script = self.source,
            inputs = inputs.to_nickel_record(),
        );
        let expression: Expr = Nickel::evaluate_source(&source, "task script")
            .map_err(|nickel_message: String| -> SindriError { SindriError::ScriptEvaluation { nickel_message } })?;
        expression.to_serde().map_err(|error| -> SindriError {
            SindriError::ScriptEvaluation {
                nickel_message: error.to_string(),
            }
        })
    }
}

/// Render `value` as a Nickel string literal, escaping the characters that would otherwise end the
/// string or be read as an escape. Used to embed Sindri-supplied paths into the assembled source.
fn nickel_string(value: &str) -> String {
    let mut escaped: String = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_set::FileSetPattern;
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

    fn evaluate(script_source: &str, files: &[&str]) -> SindriResult<Vec<Command>> {
        let input_files: FileSet = file_set(files);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
        let inputs: ScriptInputs =
            ScriptInputs::new(&input_files, &output_directory, &workspace_root, &module_directory);
        Script::new(script_source).evaluate(&inputs)
    }

    #[test]
    fn a_script_exposes_its_source() {
        assert_eq!(Script::new("fun inputs => []").source(), "fun inputs => []");
    }

    #[test]
    fn nickel_string_escapes_characters_that_would_end_or_be_read_as_an_escape() {
        assert_eq!(nickel_string("plain"), "\"plain\"");
        assert_eq!(nickel_string("a\"b\\c\nd\re\tf"), "\"a\\\"b\\\\c\\nd\\re\\tf\"");
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
    fn the_go_scripts_yield_their_toolchain_commands() {
        for (script, program, first_argument) in [
            (Script::go_format(), "gofmt", "-l"),
            (Script::go_compile(), "go", "build"),
            (Script::go_test(), "go", "test"),
        ] {
            let input_files: FileSet = file_set(&[]);
            let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
            let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
            let module_directory: RelativeDirectory = RelativeDirectory::new(MODULE);
            let inputs: ScriptInputs =
                ScriptInputs::new(&input_files, &output_directory, &workspace_root, &module_directory);
            let commands: Vec<Command> = script.evaluate(&inputs).unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].program(), program);
            assert_eq!(commands[0].arguments()[0], SmolStr::new(first_argument));
        }
    }
}
