use crate::error::SindriError;
use crate::error::SindriResult;
use crate::nickel_eval::Contract;
use crate::nickel_eval::Nickel;
use crate::parameter::ParameterState;
use crate::runtime::FileSystem;
use crate::types::{
    AbsoluteDirectory, AbsoluteFile, BuildFile, ConfigFile, DirEntry, FileKind, Language, ModuleIdentity, ModuleName,
    RelativeFile, Version, WorkspaceRoot,
};
use crate::workspace::Workspace;
use nickel_lang::Expr;
use serde::Deserialize;
use serde::de::Error as DeserializeError;
use smol_str::SmolStr;
use std::borrow::Cow;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;

const MODULE_CONTRACT: Contract = Contract::new(
    include_str!("contracts/module.ncl"),
    r#"{
  name = "my-module",
  language = "go",
  type = "executable",
  version = "0.1.0",
}"#,
);

#[derive(Debug)]
pub enum ArtifactType {
    Library,
    Executable,
    WebArchive,
    ContainerImage,
}

impl ArtifactType {
    /// Whether this is a `library` — the only type that may appear as another module's dependency.
    /// Executables, web archives, and container images may consume dependencies but never be one.
    pub fn is_library(&self) -> bool {
        matches!(self, ArtifactType::Library)
    }

    /// Whether this is an `executable` — the only type that may appear as a `module_tools` target,
    /// since only an executable module produces a binary another task can run.
    pub fn is_executable(&self) -> bool {
        matches!(self, ArtifactType::Executable)
    }

    /// The type's on-the-wire name, for diagnostics that report a module's type back to the user.
    pub fn name(&self) -> &'static str {
        match self {
            ArtifactType::Library => "library",
            ArtifactType::Executable => "executable",
            ArtifactType::WebArchive => "web-archive",
            ArtifactType::ContainerImage => "container-image",
        }
    }
}

impl<'deserialize> Deserialize<'deserialize> for ArtifactType {
    fn deserialize<Deserializer: serde::Deserializer<'deserialize>>(
        deserializer: Deserializer,
    ) -> Result<Self, Deserializer::Error> {
        let value: String = String::deserialize(deserializer)?;
        match value.as_str() {
            "library" => Ok(ArtifactType::Library),
            "executable" => Ok(ArtifactType::Executable),
            "web-archive" => Ok(ArtifactType::WebArchive),
            "container-image" => Ok(ArtifactType::ContainerImage),
            other => Err(DeserializeError::custom(format!(
                "unknown artifact type `{other}`; expected one of: library, executable, web-archive, container-image"
            ))),
        }
    }
}

/// The name of a binary a `module_tools` entry names, unique within the referenced module's
/// package output.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BinaryName(SmolStr);

impl Display for BinaryName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

/// A reference to a binary produced by an `executable` module in the same workspace, named in a
/// module's `module_tools` list. Parsed from the `//<module identity>:<binary>` label syntax — a
/// module identity, a literal `:`, then the binary name — e.g. `//tools/codegen:codegen`. Neither a
/// [`ModuleIdentity`] directory segment nor its qualifier can contain `:`, so splitting on the first
/// `:` unambiguously separates the two parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleToolReference {
    module: ModuleIdentity,
    binary: BinaryName,
}

impl ModuleToolReference {
    pub fn module(&self) -> &ModuleIdentity {
        &self.module
    }

    pub fn binary(&self) -> &BinaryName {
        &self.binary
    }

    pub fn parse(text: &str) -> Result<ModuleToolReference, ModuleToolReferenceParseError> {
        let malformed = || -> ModuleToolReferenceParseError {
            ModuleToolReferenceParseError {
                text: SmolStr::new(text),
            }
        };
        let (module_text, binary_text): (&str, &str) = text.split_once(':').ok_or_else(malformed)?;
        let module: ModuleIdentity = ModuleIdentity::parse(module_text).map_err(|_| malformed())?;
        if !Self::is_valid_binary_name(binary_text) {
            return Err(malformed());
        }
        Ok(ModuleToolReference {
            module,
            binary: BinaryName(SmolStr::new(binary_text)),
        })
    }

    fn is_valid_binary_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .bytes()
                .all(|byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }
}

impl<'deserialize> Deserialize<'deserialize> for ModuleToolReference {
    fn deserialize<Deserializer: serde::Deserializer<'deserialize>>(
        deserializer: Deserializer,
    ) -> Result<Self, Deserializer::Error> {
        let text: SmolStr = SmolStr::deserialize(deserializer)?;
        ModuleToolReference::parse(&text).map_err(DeserializeError::custom)
    }
}

/// The reason a string could not be read as a [`ModuleToolReference`]. Carries the offending text so
/// the message can point at exactly what was written.
#[derive(Clone, Debug)]
pub struct ModuleToolReferenceParseError {
    text: SmolStr,
}

impl Display for ModuleToolReferenceParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        write!(
            formatter,
            "invalid module_tools entry `{}`; expected `//path:binary` or `//path [qualifier]:binary`",
            self.text
        )
    }
}

impl std::error::Error for ModuleToolReferenceParseError {}

/// The identity of an external-artifact dependency (e.g. `example-org:some-lib`) — the artifact
/// parallel of a [`ModuleIdentity`]. A thin newtype over the coordinate text; it names which artifact,
/// not which version.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct ArtifactIdentity(SmolStr);

impl AsRef<str> for ArtifactIdentity {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// A single dependency: on a module in this workspace, or on an external artifact. Naming exactly one
/// of the two is an invariant of the type — a dependency can never name both or neither.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dependency {
    Module(ModuleIdentity),
    Artifact(ArtifactIdentity),
}

impl Dependency {
    /// The module this dependency names, or `None` when it names an external artifact. Lets the graph
    /// loader follow module edges without matching on the variant at every call site.
    pub fn module_identity(&self) -> Option<&ModuleIdentity> {
        match self {
            Dependency::Module(identity) => Some(identity),
            Dependency::Artifact(_) => None,
        }
    }
}

/// A module's declared dependencies, grouped by scope. Re-exporting a compile dependency to this
/// module's own consumers is modelled as its own [`exported`](DependencyGroup::exported) scope rather
/// than a per-dependency flag, so an exported non-compile dependency simply cannot be represented.
/// Only `module` dependencies participate in the build graph; `artifact` dependencies are recorded but
/// not resolved. Empty scopes throughout are the ordinary "no dependencies" case — the [`Default`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct DependencyGroup {
    #[serde(default)]
    compile: Vec<Dependency>,
    #[serde(default, rename = "export")]
    exported: Vec<Dependency>,
    #[serde(default)]
    test: Vec<Dependency>,
    #[serde(default)]
    runtime: Vec<Dependency>,
    #[serde(default, rename = "test-runtime")]
    test_runtime: Vec<Dependency>,
}

impl DependencyGroup {
    /// Compile-scope dependencies that are not re-exported to consumers. Everything visible during
    /// compilation is these together with [`exported`](DependencyGroup::exported).
    pub fn compile(&self) -> &[Dependency] {
        &self.compile
    }

    /// Compile-scope dependencies re-exported to this module's consumers.
    pub fn exported(&self) -> &[Dependency] {
        &self.exported
    }

    pub fn test(&self) -> &[Dependency] {
        &self.test
    }

    pub fn runtime(&self) -> &[Dependency] {
        &self.runtime
    }

    pub fn test_runtime(&self) -> &[Dependency] {
        &self.test_runtime
    }

    /// Every module named as a dependency, across all scopes, in declaration order. External
    /// `artifact` dependencies are skipped — only `module` edges participate in the build graph, so
    /// this is what the graph loader follows transitively.
    pub fn module_dependencies(&self) -> impl Iterator<Item = &ModuleIdentity> {
        self.compile
            .iter()
            .chain(&self.exported)
            .chain(&self.test)
            .chain(&self.runtime)
            .chain(&self.test_runtime)
            .filter_map(Dependency::module_identity)
    }
}

impl<'deserialize> Deserialize<'deserialize> for Dependency {
    fn deserialize<Deserializer: serde::Deserializer<'deserialize>>(
        deserializer: Deserializer,
    ) -> Result<Self, Deserializer::Error> {
        // The wire shape of a single dependency: a record naming a module or an artifact. The Nickel
        // contract already enforces that exactly one of the two is present; the match below maps the
        // record onto the variants and still treats "both" and "neither" as errors, so the invariant
        // holds even were a caller ever to evaluate without the contract.
        #[derive(Deserialize)]
        struct Fields {
            #[serde(default)]
            module: Option<ModuleIdentity>,
            #[serde(default)]
            artifact: Option<ArtifactIdentity>,
        }
        let fields: Fields = Fields::deserialize(deserializer)?;
        match (fields.module, fields.artifact) {
            (Some(module), None) => Ok(Dependency::Module(module)),
            (None, Some(artifact)) => Ok(Dependency::Artifact(artifact)),
            (Some(_), Some(_)) => Err(DeserializeError::custom(
                "a dependency must set exactly one of `module` or `artifact`, not both",
            )),
            (None, None) => Err(DeserializeError::custom(
                "a dependency must set either `module` or `artifact`",
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Module {
    name: ModuleName,
    language: Language,
    #[serde(rename = "type")]
    artifact_type: ArtifactType,
    version: Version,
    #[serde(default)]
    dependencies: DependencyGroup,
    #[serde(default)]
    parameters: ParameterState,
    #[serde(default)]
    module_tools: Vec<ModuleToolReference>,
}

impl Module {
    pub fn name(&self) -> &ModuleName {
        &self.name
    }

    pub fn language(&self) -> &Language {
        &self.language
    }

    pub fn artifact_type(&self) -> &ArtifactType {
        &self.artifact_type
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub fn dependencies(&self) -> &DependencyGroup {
        &self.dependencies
    }

    pub fn module_tools(&self) -> &[ModuleToolReference] {
        &self.module_tools
    }

    pub fn parameters(&self) -> &ParameterState {
        &self.parameters
    }
}

impl BuildFile {
    pub fn find(workspace: &Workspace, file_system: &impl FileSystem) -> SindriResult<BuildFile> {
        let workspace_root: &WorkspaceRoot = workspace.workspace_root();
        let mut current: AbsoluteDirectory = workspace.absolute_working_directory();
        let root_directory: AbsoluteDirectory = workspace_root.to_absolute_directory();
        let default_build_file_name: RelativeFile =
            RelativeFile::new("sindri.build").expect("a literal file name is always well-formed");
        loop {
            let default_build_file: AbsoluteFile = current.join_file(&default_build_file_name);
            let relative_build_file: RelativeFile = workspace_root.relativize_file(&default_build_file);
            let default_kind: Option<FileKind> =
                file_system
                    .file_kind(default_build_file.as_ref())
                    .map_err(|source| SindriError::Io {
                        path: relative_build_file.as_ref().to_path_buf(),
                        source,
                    })?;
            match default_kind {
                Some(FileKind::Symlink) => {
                    return Err(SindriError::SymlinkNotSupported {
                        path: relative_build_file.as_ref().to_path_buf(),
                    });
                }
                Some(FileKind::File) => {
                    return Ok(BuildFile::new(relative_build_file));
                }
                Some(FileKind::Directory) | None => {}
            }
            let entries: Vec<DirEntry> =
                file_system
                    .read_directory(current.as_ref())
                    .map_err(|source| SindriError::Io {
                        path: workspace_root.relativize_directory(&current).as_ref().to_path_buf(),
                        source,
                    })?;
            for entry in &entries {
                let name: Cow<'_, str> = entry.file_name();
                if name.starts_with("sindri-") && name.ends_with(".build") {
                    let entry_file: AbsoluteFile = AbsoluteFile::new(entry.path().to_path_buf());
                    let relative_entry: RelativeFile = workspace_root.relativize_file(&entry_file);
                    if entry.kind() == FileKind::Symlink {
                        return Err(SindriError::SymlinkNotSupported {
                            path: relative_entry.as_ref().to_path_buf(),
                        });
                    }
                    return Ok(BuildFile::new(relative_entry));
                }
            }
            if current == root_directory {
                break;
            }
            match current.parent() {
                Some(parent) => current = parent,
                None => break,
            }
        }
        Err(SindriError::ModuleNotFound {
            start: workspace.working_directory().as_ref().to_path_buf(),
        })
    }
}

impl Module {
    pub fn load(build_file: &BuildFile, workspace: &Workspace, file_system: &impl FileSystem) -> SindriResult<Module> {
        let config_file: ConfigFile = build_file.config_file(workspace.workspace_root());
        let expression: Expr = Nickel::evaluate_with_contract(&config_file, &MODULE_CONTRACT, file_system)?;
        expression.to_serde::<Module>().map_err(|source| SindriError::Schema {
            path: config_file.workspace_path().as_ref().to_path_buf(),
            message: source.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::SindriError;
    use crate::parameter::Parameter;
    use crate::parameter::ParameterBinding;
    use crate::parameter::ParameterDeclarations;
    use crate::parameter::ParameterName;
    use crate::parameter::ParameterType;
    use crate::parameter::PluginName;
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::types::WorkingDirectory;
    use crate::workspace::WorkspaceConfig;
    use std::io::ErrorKind;
    use std::path::Path;
    use std::path::PathBuf;

    const WORKSPACE: &str = r#"{ name = "test", sindri_version = "0.1.0" }"#;
    const MINIMAL: &str = r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0" }"#;

    fn workspace_runtime() -> DummyRuntimeBuilder {
        DummyRuntime::builder().file("/workspace/sindri.workspace", WORKSPACE)
    }

    fn make_workspace(runtime: &DummyRuntime, working_subdirectory: &str) -> Workspace {
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        let absolute_subdirectory: AbsoluteDirectory =
            AbsoluteDirectory::new(workspace_root.as_ref().join(working_subdirectory));
        let working_directory: WorkingDirectory = WorkingDirectory::derive(&absolute_subdirectory, &workspace_root);
        let config: WorkspaceConfig = WorkspaceConfig::load(&workspace_root, runtime).unwrap();
        Workspace::new(workspace_root, working_directory, config)
    }

    #[test]
    fn find_build_file_in_current_directory() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::find(&workspace, &runtime).unwrap();
        assert_eq!(build_file.as_ref(), Path::new("sindri.build"));
    }

    #[test]
    fn find_build_file_in_parent_directory() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri.build", MINIMAL)
            .directory("/workspace/src")
            .build();
        let workspace: Workspace = make_workspace(&runtime, "src");
        let build_file: BuildFile = BuildFile::find(&workspace, &runtime).unwrap();
        assert_eq!(build_file.as_ref(), Path::new("sindri.build"));
    }

    #[test]
    fn find_build_file_qualified_build_file() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri-kotlin.build", MINIMAL)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::find(&workspace, &runtime).unwrap();
        assert_eq!(build_file.as_ref(), Path::new("sindri-kotlin.build"));
    }

    #[test]
    fn find_build_file_stops_at_workspace_root() {
        let runtime: DummyRuntime = workspace_runtime().directory("/workspace/src").build();
        let workspace: Workspace = make_workspace(&runtime, "src");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::ModuleNotFound { .. }));
    }

    #[test]
    fn find_build_file_not_found() {
        let runtime: DummyRuntime = workspace_runtime().build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::ModuleNotFound { .. }));
    }

    #[test]
    fn find_build_file_rejects_symlink() {
        let runtime: DummyRuntime = workspace_runtime().symlink("/workspace/sindri.build").build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::SymlinkNotSupported { .. }));
    }

    #[test]
    fn find_build_file_rejects_qualified_build_file_symlink() {
        let runtime: DummyRuntime = workspace_runtime().symlink("/workspace/sindri-kotlin.build").build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::SymlinkNotSupported { .. }));
    }

    #[test]
    fn find_build_file_reports_io_errors_from_file_kind() {
        let runtime: DummyRuntime = workspace_runtime()
            .error("/workspace/sindri.build", ErrorKind::PermissionDenied)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }));
    }

    #[test]
    fn find_build_file_reports_io_errors_from_read_directory() {
        let runtime: DummyRuntime = workspace_runtime()
            .error("/workspace", ErrorKind::PermissionDenied)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let error: SindriError = BuildFile::find(&workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }));
    }

    #[test]
    fn load_valid_module() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        assert_eq!(module.name().as_ref(), "my-app");
        assert!(matches!(module.language(), Language::Go));
        assert!(matches!(module.artifact_type(), ArtifactType::Executable));
        assert_eq!(module.version().as_ref(), "0.1.0");
    }

    #[test]
    fn load_module_syntax_error() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri.build", r#"{ name = "test", language = }"#)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_missing_required_field() {
        let runtime: DummyRuntime = workspace_runtime()
            .file("/workspace/sindri.build", r#"{ name = "test", language = "go" }"#)
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_contract_type_violation() {
        let runtime: DummyRuntime = workspace_runtime()
            .file(
                "/workspace/sindri.build",
                r#"{ name = 42, language = "go", type = "executable", version = "0.1.0" }"#,
            )
            .build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(matches!(error, SindriError::NickelEval { .. }));
    }

    #[test]
    fn load_module_passes_contract() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        Module::load(&build_file, &workspace, &runtime).unwrap();
    }

    #[test]
    fn load_module_groups_dependencies_by_scope() {
        let source: &str = r#"{
  name = "app",
  language = "go",
  type = "executable",
  version = "0.1.0",
  dependencies = {
    compile = [
      { module = "//libs/common" },
      { artifact = "example-org:some-lib" },
    ],
    export = [
      { module = "//libs/api" },
    ],
    test = [
      { module = "//libs/test-helpers" },
    ],
  },
}"#;
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", source).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        let dependencies: &DependencyGroup = module.dependencies();
        // The non-exported compile entries: the module and the artifact.
        assert_eq!(dependencies.compile().len(), 2);
        assert_eq!(
            dependencies.compile()[0].module_identity().map(ToString::to_string),
            Some("//libs/common/".to_string())
        );
        match &dependencies.compile()[1] {
            Dependency::Artifact(identity) => assert_eq!(identity.as_ref(), "example-org:some-lib"),
            other => panic!("expected an artifact dependency, got {other:?}"),
        }
        // The exported compile entry lands in its own scope.
        assert_eq!(dependencies.exported().len(), 1);
        assert_eq!(
            dependencies.exported()[0].module_identity().map(ToString::to_string),
            Some("//libs/api/".to_string())
        );
        assert_eq!(dependencies.test().len(), 1);
        assert!(dependencies.runtime().is_empty());
        assert!(dependencies.test_runtime().is_empty());
    }

    #[test]
    fn load_module_parses_parameters() {
        let source: &str = r#"{
  name = "app", language = "go", type = "executable", version = "0.1.0",
  parameters = { "sindri-go" = { mode = "release" } },
}"#;
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", source).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        let declared: ParameterDeclarations = ParameterDeclarations::new([Parameter::new(
            PluginName::new("sindri-go"),
            ParameterName::new("mode"),
            ParameterType::new("String"),
        )]);
        let binding: ParameterBinding = ParameterBinding::resolve(&declared, module.parameters()).unwrap();
        assert_eq!(
            binding.to_nickel_record(),
            r#"{ "sindri-go" = { "mode" = "release" } }"#
        );
    }

    #[test]
    fn load_module_without_parameters_has_an_empty_set() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        let declared: ParameterDeclarations = ParameterDeclarations::default();
        assert!(ParameterBinding::resolve(&declared, module.parameters()).is_ok());
    }

    #[test]
    fn load_module_without_dependencies_has_empty_scopes() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        let dependencies: &DependencyGroup = module.dependencies();
        assert!(dependencies.compile().is_empty());
        assert!(dependencies.exported().is_empty());
        assert!(dependencies.test().is_empty());
        assert!(dependencies.runtime().is_empty());
        assert!(dependencies.test_runtime().is_empty());
    }

    #[test]
    fn per_entry_export_field_is_a_contract_error() {
        // `export` is a scope, not a per-dependency flag: re-exported compile dependencies go in the
        // `export` scope. A stray `export` on an entry is an unknown field on the closed dependency
        // contract, so it fails during Nickel evaluation.
        let source: &str = r#"{
  name = "app", language = "go", type = "executable", version = "0.1.0",
  dependencies = { compile = [ { module = "//libs/common", export = true } ] },
}"#;
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", source).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(
            matches!(error, SindriError::NickelEval { .. }),
            "expected NickelEval, got {error:?}"
        );
    }

    #[test]
    fn artifact_type_deserializes_every_kind() {
        assert!(matches!(
            serde_json::from_str::<ArtifactType>(r#""library""#).unwrap(),
            ArtifactType::Library
        ));
        assert!(matches!(
            serde_json::from_str::<ArtifactType>(r#""executable""#).unwrap(),
            ArtifactType::Executable
        ));
        assert!(matches!(
            serde_json::from_str::<ArtifactType>(r#""web-archive""#).unwrap(),
            ArtifactType::WebArchive
        ));
        assert!(matches!(
            serde_json::from_str::<ArtifactType>(r#""container-image""#).unwrap(),
            ArtifactType::ContainerImage
        ));
    }

    #[test]
    fn artifact_type_rejects_an_unknown_kind() {
        assert!(serde_json::from_str::<ArtifactType>(r#""firmware""#).is_err());
    }

    #[test]
    fn dependency_rejects_setting_both_module_and_artifact() {
        let both: &str = r#"{ "module": "//libs/common", "artifact": "example-org:some-lib" }"#;
        assert!(serde_json::from_str::<Dependency>(both).is_err());
    }

    #[test]
    fn dependency_rejects_setting_neither_module_nor_artifact() {
        let neither: &str = r#"{}"#;
        assert!(serde_json::from_str::<Dependency>(neither).is_err());
    }

    #[test]
    fn artifact_type_name_reports_the_wire_name_for_every_kind() {
        assert_eq!(ArtifactType::Library.name(), "library");
        assert_eq!(ArtifactType::Executable.name(), "executable");
        assert_eq!(ArtifactType::WebArchive.name(), "web-archive");
        assert_eq!(ArtifactType::ContainerImage.name(), "container-image");
    }

    #[test]
    fn artifact_dependency_has_no_module_identity() {
        let artifact: Dependency = Dependency::Artifact(ArtifactIdentity(SmolStr::new("example-org:some-lib")));
        assert_eq!(artifact.module_identity(), None);
    }

    #[test]
    fn artifact_type_is_executable_true_only_for_executable() {
        assert!(!ArtifactType::Library.is_executable());
        assert!(ArtifactType::Executable.is_executable());
        assert!(!ArtifactType::WebArchive.is_executable());
        assert!(!ArtifactType::ContainerImage.is_executable());
    }

    #[test]
    fn module_tool_reference_parses_a_label() {
        let reference: ModuleToolReference = ModuleToolReference::parse("//tools/codegen:codegen").unwrap();
        assert_eq!(reference.module().to_string(), "//tools/codegen/");
        assert_eq!(reference.binary().to_string(), "codegen");
    }

    #[test]
    fn module_tool_reference_parses_a_label_with_a_qualifier() {
        let reference: ModuleToolReference = ModuleToolReference::parse("//tools/codegen [bin]:codegen").unwrap();
        assert_eq!(reference.module().to_string(), "//tools/codegen/ [bin]");
        assert_eq!(reference.binary().to_string(), "codegen");
    }

    #[test]
    fn module_tool_reference_rejects_a_missing_colon() {
        assert!(ModuleToolReference::parse("//tools/codegen").is_err());
    }

    #[test]
    fn module_tool_reference_rejects_an_empty_binary_name() {
        assert!(ModuleToolReference::parse("//tools/codegen:").is_err());
    }

    #[test]
    fn module_tool_reference_rejects_a_malformed_module_part() {
        assert!(ModuleToolReference::parse("tools/codegen:codegen").is_err());
    }

    #[test]
    fn module_tool_reference_deserializes_from_a_string() {
        let reference: ModuleToolReference = serde_json::from_str(r#""//tools/codegen:codegen""#).unwrap();
        assert_eq!(reference.binary().to_string(), "codegen");
    }

    #[test]
    fn load_module_parses_module_tools() {
        let source: &str = r#"{
  name = "app", language = "go", type = "executable", version = "0.1.0",
  module_tools = [ "//tools/codegen:codegen" ],
}"#;
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", source).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        assert_eq!(module.module_tools().len(), 1);
        assert_eq!(module.module_tools()[0].binary().to_string(), "codegen");
    }

    #[test]
    fn load_module_without_module_tools_has_an_empty_list() {
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", MINIMAL).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let module: Module = Module::load(&build_file, &workspace, &runtime).unwrap();
        assert!(module.module_tools().is_empty());
    }

    #[test]
    fn load_module_rejects_a_malformed_module_tools_label() {
        let source: &str = r#"{
  name = "app", language = "go", type = "executable", version = "0.1.0",
  module_tools = [ "not-a-label" ],
}"#;
        let runtime: DummyRuntime = workspace_runtime().file("/workspace/sindri.build", source).build();
        let workspace: Workspace = make_workspace(&runtime, "");
        let build_file: BuildFile = BuildFile::new(RelativeFile::new_unchecked("sindri.build"));
        let error: SindriError = Module::load(&build_file, &workspace, &runtime).unwrap_err();
        assert!(
            matches!(error, SindriError::Schema { .. }),
            "expected Schema, got {error:?}"
        );
    }
}
