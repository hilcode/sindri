use crate::error::SindriError;
use crate::error::SindriResult;
use crate::file_set::FileSet;
use crate::module::BinaryName;
use crate::nickel_eval::Nickel;
use crate::nickel_import::ScriptResolutionState;
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
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;

/// The Nickel contract every command a script yields is checked against. Applied by Sindri, it makes
/// `program` required, fills the `arguments`/`environment` defaults, and defaults `working-directory`
/// to the module directory Sindri supplies.
const COMMAND_CONTRACT: &str = include_str!("contracts/command.ncl");

/// Shared Nickel contract definitions (currently just `RelativeDirectory`) available to
/// [`COMMAND_CONTRACT`]. A `let ... in` fragment, not a standalone expression, so it splices
/// directly in front of the contract text it scopes over — see [`Script::evaluate`].
const STDLIB: &str = include_str!("contracts/stdlib.ncl");

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

    /// Build a command directly from its parts, bypassing script evaluation and contract
    /// validation. Test-only: production code only ever obtains a `Command` by evaluating a
    /// [`Script`] against the [`COMMAND_CONTRACT`].
    #[cfg(test)]
    pub fn new(program: impl Into<SmolStr>, arguments: impl IntoIterator<Item = impl Into<SmolStr>>) -> Command {
        Command {
            program: program.into(),
            arguments: arguments.into_iter().map(Into::into).collect(),
            environment: BTreeMap::new(),
            working_directory: RelativeDirectory::new_unchecked(""),
        }
    }
}

/// Renders the command as a single space-joined line for diagnostics and logs. This is a display
/// convenience only — execution always uses the structured program and arguments, so an argument
/// containing spaces is never re-split.
impl Display for Command {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.program)?;
        for argument in &self.arguments {
            write!(formatter, " {argument}")?;
        }
        Ok(())
    }
}

/// The values Sindri supplies to a [`Script`] expression: the task's bound parameters, the resolved
/// input files, the task's output directory, the base its managed input is resolved against, the
/// workspace root, the module directory each command's `working-directory` defaults to, and the
/// resolved location of every `module_tools` binary this task references. Paths reach the script as
/// strings — workspace-relative for `files`, absolute for the directories and the module-tools binary
/// paths — since a script computes with them but never reads them.
pub struct ScriptInputs<'inputs> {
    parameters: &'inputs ParameterBinding,
    input_files: &'inputs FileSet,
    output_directory: &'inputs AbsoluteDirectory,
    managed_input_base: &'inputs AbsoluteDirectory,
    workspace_root: &'inputs WorkspaceRoot,
    module_directory: &'inputs RelativeDirectory,
    module_tools: &'inputs BTreeMap<BinaryName, AbsoluteFile>,
}

impl<'inputs> ScriptInputs<'inputs> {
    pub fn new(
        parameters: &'inputs ParameterBinding,
        input_files: &'inputs FileSet,
        output_directory: &'inputs AbsoluteDirectory,
        managed_input_base: &'inputs AbsoluteDirectory,
        workspace_root: &'inputs WorkspaceRoot,
        module_directory: &'inputs RelativeDirectory,
        module_tools: &'inputs BTreeMap<BinaryName, AbsoluteFile>,
    ) -> ScriptInputs<'inputs> {
        ScriptInputs {
            parameters,
            input_files,
            output_directory,
            managed_input_base,
            workspace_root,
            module_directory,
            module_tools,
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
        let relative_output_directory: RelativeDirectory =
            self.workspace_root.relativize_directory(self.output_directory);
        let module_tools: Vec<String> = self
            .module_tools
            .iter()
            .map(|(binary, file): (&BinaryName, &AbsoluteFile)| -> String {
                format!("\"{binary}\" = {}", Nickel::string_literal(&file.to_string()))
            })
            .collect();
        format!(
            "{{ params = {parameters}, files = [ {files} ], \"output-directory\" = {output_directory}, \
             \"working-directory\" = {relative_output_directory}, \"managed-input-directory\" = {managed_input_base}, \
             \"workspace-root\" = {workspace_root}, \"module-tools\" = {{ {module_tools} }} }}",
            parameters = self.parameters.to_nickel_record(),
            files = files.join(", "),
            output_directory = Nickel::string_literal(&self.output_directory.to_string()),
            relative_output_directory = Nickel::string_literal(&relative_output_directory.to_string()),
            managed_input_base = Nickel::string_literal(&self.managed_input_base.to_string()),
            workspace_root = Nickel::string_literal(&self.workspace_root.to_string()),
            module_tools = module_tools.join(", "),
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

    pub fn go_package() -> Script {
        Script::new(include_str!("scripts/go-package.ncl"))
    }

    pub fn go_test() -> Script {
        Script::new(include_str!("scripts/go-test.ncl"))
    }

    /// The `generate-go-work` script: (re)creates a `go.work` covering exactly `module_directories`
    /// in the task's own output directory, via the real `go` toolchain rather than Sindri hand-writing
    /// the file — so the `go` directive and file format are always whatever the running toolchain
    /// produces. Unlike the other shipped scripts, this one is not a static file: the module
    /// directories vary per build and per workspace, so they are embedded straight into the source
    /// this assembles, the same way [`Nickel::string_literal`] is meant for. That also gives this task
    /// its dirtiness tracking for free — no declared or managed input is needed, since the module set
    /// changing is exactly a change to the script's own source, and hence to its definition hash.
    /// `go work init` refuses to run if a `go.work` already exists, so a stale one from a previous
    /// build (in the same output directory, since this task's binding never changes) is removed first.
    pub fn go_work(module_directories: &[String]) -> Script {
        let directories: String = module_directories
            .iter()
            .map(|directory: &String| -> String { Nickel::string_literal(directory) })
            .collect::<Vec<String>>()
            .join(", ");
        Script::new(format!(
            "fun inputs => let directory = inputs.\"working-directory\" in [ \
             {{ program = \"rm\", arguments = [ \"-f\", \"go.work\" ], \"working-directory\" = directory }}, \
             {{ program = \"go\", arguments = [ \"work\", \"init\" ] @ [ {directories} ], \"working-directory\" = directory }} ]"
        ))
    }

    /// The `clean` task's script: removes every top-level entry under the build directory except the
    /// task's own state (already excluded from `inputs.files` by its managed input pattern — see
    /// [`crate::lifecycle::Lifecycle::run_clean`]). A plain static file like every other shipped
    /// script — see `scripts/clean.ncl` for how it derives the directories to remove from
    /// `inputs.files`.
    pub fn clean() -> Script {
        Script::new(include_str!("scripts/clean.ncl"))
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
        resolution_state: &mut ScriptResolutionState,
        file_system: &impl FileSystem,
    ) -> SindriResult<Vec<Command>> {
        let source: String = format!(
            "{stdlib}\n\
             let Command = ({command_contract}) {module_directory} in\n\
             let script = ({script}) in\n\
             std.array.map (fun command => command | Command) (script {inputs})",
            stdlib = STDLIB,
            command_contract = COMMAND_CONTRACT,
            module_directory = Nickel::string_literal(&inputs.module_directory.to_string()),
            script = self.source,
            inputs = inputs.to_nickel_record(),
        );
        let value: NickelValue = evaluate_hermetically(
            &source,
            script_path,
            inputs.workspace_root,
            resolution_state,
            file_system,
        )?;
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
    use crate::parameter::ParameterState;
    use crate::parameter::ParameterType;
    use crate::parameter::ParameterValue;
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

    fn no_module_tools() -> BTreeMap<BinaryName, AbsoluteFile> {
        BTreeMap::new()
    }

    fn evaluate(script_source: &str, files: &[&str]) -> SindriResult<Vec<Command>> {
        let parameters: ParameterBinding = ParameterBinding::empty();
        let input_files: FileSet = file_set(files);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let managed_input_base: AbsoluteDirectory =
            AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new_unchecked(MODULE);
        let module_tools: BTreeMap<BinaryName, AbsoluteFile> = no_module_tools();
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &managed_input_base,
            &workspace_root,
            &module_directory,
            &module_tools,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        Script::new(script_source).evaluate(&inputs, &script_path(), &mut resolution_state, &runtime)
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
    fn a_script_yielding_an_absolute_working_directory_is_a_contract_error() {
        let result: SindriResult<Vec<Command>> = evaluate(
            "fun inputs => [ { program = \"go\", \"working-directory\" = \"/etc\" } ]",
            &[],
        );
        let error: SindriError = result.unwrap_err();
        assert!(matches!(error, SindriError::ScriptEvaluation { .. }));
        assert!(
            error
                .to_string()
                .contains("must be a relative directory (must not start with `/`)"),
            "message was: {error}"
        );
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
    fn a_script_can_read_its_relative_working_directory() {
        let commands: Vec<Command> = evaluate(
            "fun inputs => [ { program = \"echo\", arguments = [ inputs.\"working-directory\" ] } ]",
            &[],
        )
        .unwrap();
        assert_eq!(commands[0].arguments(), &[SmolStr::new(".target/out/")]);
    }

    #[test]
    fn a_script_can_read_its_managed_input_directory() {
        let commands: Vec<Command> = evaluate(
            "fun inputs => [ { program = \"echo\", arguments = [ inputs.\"managed-input-directory\" ] } ]",
            &[],
        )
        .unwrap();
        assert_eq!(
            commands[0].arguments(),
            &[SmolStr::new("/workspace/.target/generate-go-work/binding/")]
        );
    }

    #[test]
    fn a_script_can_import_a_workspace_local_helper() {
        let parameters: ParameterBinding = ParameterBinding::empty();
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let managed_input_base: AbsoluteDirectory =
            AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new_unchecked(MODULE);
        let module_tools: BTreeMap<BinaryName, AbsoluteFile> = no_module_tools();
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &managed_input_base,
            &workspace_root,
            &module_directory,
            &module_tools,
        );
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(format!("{WORKSPACE}/{MODULE}/helper.ncl"), "\"go\"")
            .build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let commands: Vec<Command> = Script::new("fun inputs => [ { program = import \"helper.ncl\" } ]")
            .evaluate(&inputs, &script_path(), &mut resolution_state, &runtime)
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
        let values: ParameterState = ParameterState::new([(
            PluginName::new("plugin"),
            ParameterName::new("mode"),
            ParameterValue::new("\"debug\""),
        )]);
        let parameters: ParameterBinding = ParameterBinding::resolve(&declared, &values).unwrap();
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let managed_input_base: AbsoluteDirectory =
            AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new_unchecked(MODULE);
        let module_tools: BTreeMap<BinaryName, AbsoluteFile> = no_module_tools();
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &managed_input_base,
            &workspace_root,
            &module_directory,
            &module_tools,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let commands: Vec<Command> =
            Script::new("fun inputs => [ { program = \"echo\", arguments = [ inputs.params.\"plugin\".mode ] } ]")
                .evaluate(&inputs, &script_path(), &mut resolution_state, &runtime)
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
        let values: ParameterState = ParameterState::new([(
            PluginName::new("plugin"),
            ParameterName::new("mode"),
            ParameterValue::new("\"debug\""),
        )]);
        let parameters: ParameterBinding = ParameterBinding::resolve(&declared, &values).unwrap();
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let managed_input_base: AbsoluteDirectory =
            AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new_unchecked(MODULE);
        let module_tools: BTreeMap<BinaryName, AbsoluteFile> = no_module_tools();
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &managed_input_base,
            &workspace_root,
            &module_directory,
            &module_tools,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let result: SindriResult<Vec<Command>> = Script::new(
            "fun inputs => [ { program = \"echo\", arguments = [ inputs.params.verbose ] } ]",
        )
        .evaluate(&inputs, &script_path(), &mut resolution_state, &runtime);
        assert!(matches!(result, Err(SindriError::ScriptEvaluation { .. })));
    }

    /// A resolved `mode` binding, the same shape `go-compile`'s declared parameter now requires before
    /// its script will evaluate — `go-format`/`go-test` still take [`ParameterBinding::empty`], since
    /// they declare no parameters at all.
    fn mode_binding(mode: &str) -> ParameterBinding {
        let declared: ParameterDeclarations = ParameterDeclarations::new([Parameter::new(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterType::new("String"),
        )]);
        let values: ParameterState = ParameterState::new([(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterValue::new(format!("\"{mode}\"")),
        )]);
        ParameterBinding::resolve(&declared, &values).unwrap()
    }

    #[test]
    fn the_go_scripts_yield_their_toolchain_commands() {
        for (script, program, first_argument, parameters) in [
            (Script::go_format(), "gofmt", "-l", ParameterBinding::empty()),
            (Script::go_compile(), "go", "build", mode_binding("debug")),
            (Script::go_package(), "go", "build", mode_binding("debug")),
            (Script::go_test(), "go", "test", ParameterBinding::empty()),
        ] {
            let input_files: FileSet = file_set(&[]);
            let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
            let managed_input_base: AbsoluteDirectory =
                AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"));
            let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
            let module_directory: RelativeDirectory = RelativeDirectory::new_unchecked(MODULE);
            let module_tools: BTreeMap<BinaryName, AbsoluteFile> = no_module_tools();
            let inputs: ScriptInputs = ScriptInputs::new(
                &parameters,
                &input_files,
                &output_directory,
                &managed_input_base,
                &workspace_root,
                &module_directory,
                &module_tools,
            );
            let runtime: DummyRuntime = DummyRuntime::builder().build();
            let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
            let commands: Vec<Command> = script
                .evaluate(&inputs, &script_path(), &mut resolution_state, &runtime)
                .unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].program(), program);
            assert_eq!(commands[0].arguments()[0], SmolStr::new(first_argument));
        }
    }

    #[test]
    fn go_package_writes_its_binary_to_the_output_directory() {
        let parameters: ParameterBinding = mode_binding("debug");
        let input_files: FileSet = file_set(&[]);
        let output_directory: AbsoluteDirectory = AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out"));
        let managed_input_base: AbsoluteDirectory =
            AbsoluteDirectory::new(PathBuf::from("/workspace/.target/generate-go-work/binding"));
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from(WORKSPACE)));
        let module_directory: RelativeDirectory = RelativeDirectory::new_unchecked(MODULE);
        let module_tools: BTreeMap<BinaryName, AbsoluteFile> = no_module_tools();
        let inputs: ScriptInputs = ScriptInputs::new(
            &parameters,
            &input_files,
            &output_directory,
            &managed_input_base,
            &workspace_root,
            &module_directory,
            &module_tools,
        );
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let mut resolution_state: ScriptResolutionState = ScriptResolutionState::new();
        let commands: Vec<Command> = Script::go_package()
            .evaluate(&inputs, &script_path(), &mut resolution_state, &runtime)
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
