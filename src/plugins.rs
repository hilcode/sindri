use crate::checksums::Checksum;
use crate::checksums::Checksums;
use crate::error::SindriError;
use crate::error::SindriResult;
use crate::file_set::FileSetPattern;
use crate::parameter::Parameter;
use crate::parameter::ParameterDeclarations;
use crate::parameter::ParameterName;
use crate::parameter::ParameterType;
use crate::parameter::PluginName;
use crate::plugin::Plugin;
use crate::runtime::FileSystem;
use crate::script::Script;
use crate::task::DeclaredTaskInput;
use crate::task::ManagedTaskInput;
use crate::task::Task;
use crate::task::TaskName;
use crate::task::TaskOutput;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::RelativeDirectory;
use crate::types::RelativeFile;
use crate::types::Step;
use crate::types::WorkspaceRoot;
use crate::workspace::Workspace;
use serde::Deserialize;
use std::collections::BTreeMap;

/// A known plugin file's path relative to the plugin's own directory (`manifest.json`,
/// `scripts/go-format.ncl`, …) and the content this binary ships for it.
struct KnownPluginFile {
    relative_path: &'static str,
    content: &'static str,
}

/// A plugin's manifest file name, relative to the plugin's own directory — the one known file
/// among a plugin's own files that [`load_known_plugin`] parses into that plugin's tasks.
const MANIFEST_RELATIVE_PATH: &str = "manifest.json";

struct KnownPlugin {
    directory_name: &'static str,
    plugin_name: &'static str,
    files: &'static [KnownPluginFile],
}

/// The plugins this version of Sindri manages, by fixed name — a second real plugin/language is
/// future work.
const KNOWN_PLUGINS: [KnownPlugin; 1] = [KnownPlugin {
    directory_name: "go",
    plugin_name: "go",
    files: &[
        KnownPluginFile {
            relative_path: MANIFEST_RELATIVE_PATH,
            content: include_str!("plugins/go/manifest.json"),
        },
        KnownPluginFile {
            relative_path: "scripts/go-format.ncl",
            content: include_str!("scripts/go-format.ncl"),
        },
        KnownPluginFile {
            relative_path: "scripts/go-compile.ncl",
            content: include_str!("scripts/go-compile.ncl"),
        },
        KnownPluginFile {
            relative_path: "scripts/go-package.ncl",
            content: include_str!("scripts/go-package.ncl"),
        },
        KnownPluginFile {
            relative_path: "scripts/go-test.ncl",
            content: include_str!("scripts/go-test.ncl"),
        },
    ],
}];

/// A plugin manifest's task list — the JSON shape `.sindri/plugins/<plugin>/manifest.json` is
/// authored in.
#[derive(Deserialize)]
struct PluginManifest {
    tasks: Vec<TaskManifest>,
}

/// One manifest-authored task: the step it binds to, its script's path relative to the plugin
/// directory, its input/output glob patterns, and its declared parameters. Mirrors [`Task`] and
/// [`Step`] field-for-field, in a shape `serde_json` can parse directly.
#[derive(Deserialize)]
struct TaskManifest {
    name: String,
    step: String,
    script: String,
    #[serde(default)]
    declared_input: Vec<String>,
    #[serde(default)]
    managed_input: Vec<String>,
    #[serde(default)]
    output: Vec<String>,
    #[serde(default)]
    parameters: Vec<ParameterManifest>,
}

#[derive(Deserialize)]
struct ParameterManifest {
    plugin: String,
    name: String,
    #[serde(rename = "type")]
    parameter_type: String,
}

fn task_from_manifest(manifest: &TaskManifest, script_content: &str) -> (Task, Step) {
    let declared_parameters: ParameterDeclarations = ParameterDeclarations::new(manifest.parameters.iter().map(
        |parameter: &ParameterManifest| -> Parameter {
            Parameter::new(
                PluginName::new(parameter.plugin.as_str()),
                ParameterName::new(parameter.name.as_str()),
                ParameterType::new(parameter.parameter_type.as_str()),
            )
        },
    ));
    let task: Task = Task::new(
        TaskName::new(manifest.name.as_str()),
        Script::new(script_content),
        DeclaredTaskInput::new(FileSetPattern::new(manifest.declared_input.iter().map(String::as_str))),
        ManagedTaskInput::new(FileSetPattern::new(manifest.managed_input.iter().map(String::as_str))),
        TaskOutput::new(FileSetPattern::new(manifest.output.iter().map(String::as_str))),
        declared_parameters,
    );
    (task, Step::new(manifest.step.as_str()))
}

/// Seed `known_file` under `plugin_directory` when it's missing or unrecorded, then verify its
/// current content against `checksums` (freshly seeded or pre-existing) and return that content.
/// Sets `*checksums_changed` when a seed adds a new checksum entry.
fn load_known_file(
    known_file: &KnownPluginFile,
    checksum_key: &RelativeFile,
    plugin_directory: &AbsoluteDirectory,
    checksums: &mut Checksums,
    checksums_changed: &mut bool,
    workspace_root: &WorkspaceRoot,
    file_system: &impl FileSystem,
) -> SindriResult<String> {
    let file: AbsoluteFile = plugin_directory.join_file(
        &RelativeFile::new(known_file.relative_path).expect("a known plugin file's path is always well-formed"),
    );
    let exists: bool = file_system
        .file_kind(file.as_ref())
        .map_err(|source| SindriError::Io {
            path: workspace_root.relative_path_buf(&file),
            source,
        })?
        .is_some();
    let content: String = if !exists || checksums.get(checksum_key).is_none() {
        if let Some(parent) = file.parent() {
            file_system
                .create_directories(parent.as_ref())
                .map_err(|source| SindriError::Io {
                    path: workspace_root.relative_directory_path_buf(&parent),
                    source,
                })?;
        }
        file_system
            .write(file.as_ref(), known_file.content.as_bytes())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relative_path_buf(&file),
                source,
            })?;
        checksums.insert(checksum_key.clone(), Checksum::of(known_file.content.as_bytes()));
        *checksums_changed = true;
        known_file.content.to_string()
    } else {
        file_system
            .read_to_string(file.as_ref())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relative_path_buf(&file),
                source,
            })?
    };
    let actual_checksum: Checksum = Checksum::of(content.as_bytes());
    if checksums.get(checksum_key) != Some(&actual_checksum) {
        return Err(SindriError::PluginModified {
            path: workspace_root.relative_path_buf(&file),
        });
    }
    Ok(content)
}

/// Seed-and-verify every one of `known`'s files under `plugins_directory`, then parse its manifest
/// and the tasks it names into a [`Plugin`].
fn load_known_plugin(
    known: &KnownPlugin,
    plugins_directory: &AbsoluteDirectory,
    checksums: &mut Checksums,
    checksums_changed: &mut bool,
    workspace_root: &WorkspaceRoot,
    file_system: &impl FileSystem,
) -> SindriResult<Plugin> {
    let plugin_directory: AbsoluteDirectory = plugins_directory.join_directory(
        &RelativeDirectory::new(format!("{}/", known.directory_name))
            .expect("a known plugin's directory name is always well-formed"),
    );
    let mut contents: BTreeMap<&str, String> = BTreeMap::new();
    for known_file in known.files {
        let checksum_key: RelativeFile =
            RelativeFile::new(format!("{}/{}", known.directory_name, known_file.relative_path))
                .expect("a known plugin file's path is always well-formed");
        let content: String = load_known_file(
            known_file,
            &checksum_key,
            &plugin_directory,
            checksums,
            checksums_changed,
            workspace_root,
            file_system,
        )?;
        contents.insert(known_file.relative_path, content);
    }
    let manifest_file: AbsoluteFile = plugin_directory
        .join_file(&RelativeFile::new(MANIFEST_RELATIVE_PATH).expect("a literal file name is always well-formed"));
    let manifest_content: &str = contents
        .get(MANIFEST_RELATIVE_PATH)
        .expect("every known plugin ships a manifest file");
    let manifest: PluginManifest = serde_json::from_str(manifest_content).map_err(|source| SindriError::Schema {
        path: workspace_root.relative_path_buf(&manifest_file),
        message: source.to_string(),
    })?;
    let tasks: Vec<(Task, Step)> = manifest
        .tasks
        .iter()
        .map(|task_manifest: &TaskManifest| -> (Task, Step) {
            let script_content: &str = contents
                .get(task_manifest.script.as_str())
                .expect("a manifest's script path always names one of the plugin's own known files");
            task_from_manifest(task_manifest, script_content)
        })
        .collect();
    Ok(Plugin::new(PluginName::new(known.plugin_name), tasks))
}

/// A workspace's loaded plugins — currently always exactly `go`, bootstrapped into
/// `.sindri/plugins/` from content embedded in this binary the first time a known file is
/// missing, and checksum-verified against that same content on every later load. Editing these
/// files is not yet a supported operation — a mismatch is a hard error, not a silently honored
/// edit.
#[derive(Debug)]
pub struct PluginRegistry {
    plugins: Vec<Plugin>,
}

impl PluginRegistry {
    /// Bootstrap-and-load `.sindri/plugins/` under `workspace`: create the directory and seed any
    /// of the known plugins' files (plus its checksum) that's missing, then verify every known
    /// file's current content against its recorded checksum before parsing each plugin's manifest.
    pub fn load(workspace: &Workspace, file_system: &impl FileSystem) -> SindriResult<PluginRegistry> {
        let workspace_root: &WorkspaceRoot = workspace.workspace_root();
        let plugins_directory: AbsoluteDirectory = workspace.absolute_sindri_directory().join_directory(
            &RelativeDirectory::new("plugins/").expect("a literal directory name is always well-formed"),
        );
        file_system
            .create_directories(plugins_directory.as_ref())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relative_directory_path_buf(&plugins_directory),
                source,
            })?;
        let mut checksums: Checksums = Checksums::load(&plugins_directory, workspace_root, file_system)?;
        let mut checksums_changed: bool = false;
        let mut plugins: Vec<Plugin> = Vec::with_capacity(KNOWN_PLUGINS.len());
        for known in &KNOWN_PLUGINS {
            plugins.push(load_known_plugin(
                known,
                &plugins_directory,
                &mut checksums,
                &mut checksums_changed,
                workspace_root,
                file_system,
            )?);
        }
        if checksums_changed {
            checksums.save(&plugins_directory, workspace_root, file_system)?;
        }
        Ok(PluginRegistry { plugins })
    }

    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::runtime::DummyRuntimeBuilder;
    use crate::workspace::Workspace;

    /// The `go` plugin's known files, indexed the way [`KNOWN_PLUGINS`] itself carries them —
    /// `manifest.json` first, then every script.
    fn go_files() -> &'static [KnownPluginFile] {
        KNOWN_PLUGINS[0].files
    }

    fn known_file(relative_path: &str) -> &'static KnownPluginFile {
        go_files()
            .iter()
            .find(|file: &&KnownPluginFile| file.relative_path == relative_path)
            .expect("a known plugin file by this name")
    }

    fn minimal_workspace() -> DummyRuntimeBuilder {
        DummyRuntime::builder()
            .file(
                "/workspace/sindri.workspace",
                r#"{ name = "test", sindri_version = "0.1.0" }"#,
            )
            .current_directory("/workspace")
    }

    fn workspace(runtime: &DummyRuntime) -> Workspace {
        Workspace::locate(runtime).unwrap()
    }

    #[test]
    fn load_seeds_every_known_file_and_a_checksum_manifest_when_missing() {
        let runtime: DummyRuntime = minimal_workspace().build();
        let registry: PluginRegistry = PluginRegistry::load(&workspace(&runtime), &runtime).unwrap();
        for file in go_files() {
            assert_eq!(
                runtime
                    .written_file(format!("/workspace/.sindri/plugins/go/{}", file.relative_path))
                    .unwrap(),
                file.content.as_bytes(),
                "expected {} to be seeded",
                file.relative_path
            );
        }
        assert!(
            runtime
                .written_file("/workspace/.sindri/plugins/checksums.json")
                .is_some(),
            "expected a checksum manifest to be written"
        );
        assert_eq!(registry.plugins().len(), 1);
        let go: &Plugin = &registry.plugins()[0];
        assert_eq!(go.name(), &PluginName::new("go"));
        let task_names: Vec<String> = go
            .tasks()
            .iter()
            .map(|(task, _): &(Task, Step)| -> String { task.name().to_string() })
            .collect();
        assert_eq!(task_names, vec!["go-format", "go-compile", "go-package", "go-test"]);
    }

    #[test]
    fn load_recreates_only_the_missing_file_and_leaves_valid_siblings_untouched() {
        let mut builder: DummyRuntimeBuilder = minimal_workspace();
        let mut checksum_entries: Vec<String> = Vec::new();
        for file in go_files() {
            if file.relative_path == "scripts/go-test.ncl" {
                continue;
            }
            builder = builder.file(
                format!("/workspace/.sindri/plugins/go/{}", file.relative_path),
                file.content,
            );
            checksum_entries.push(format!(
                r#""go/{}": "{}""#,
                file.relative_path,
                Checksum::of(file.content.as_bytes()).to_hex()
            ));
        }
        builder = builder.file(
            "/workspace/.sindri/plugins/checksums.json",
            format!("{{{}}}", checksum_entries.join(", ")),
        );
        let runtime: DummyRuntime = builder.build();
        let registry: PluginRegistry = PluginRegistry::load(&workspace(&runtime), &runtime).unwrap();
        assert_eq!(
            runtime
                .written_file("/workspace/.sindri/plugins/go/scripts/go-test.ncl")
                .unwrap(),
            known_file("scripts/go-test.ncl").content.as_bytes()
        );
        for file in go_files() {
            if file.relative_path == "scripts/go-test.ncl" {
                continue;
            }
            assert!(
                runtime
                    .written_file(format!("/workspace/.sindri/plugins/go/{}", file.relative_path))
                    .is_none(),
                "{} was already valid and should not have been rewritten",
                file.relative_path
            );
        }
        assert_eq!(registry.plugins()[0].tasks().len(), 4);
    }

    #[test]
    fn load_reads_a_pre_seeded_checksum_matching_plugin_without_rewriting_anything() {
        let mut builder: DummyRuntimeBuilder = minimal_workspace();
        let mut checksum_entries: Vec<String> = Vec::new();
        for file in go_files() {
            builder = builder.file(
                format!("/workspace/.sindri/plugins/go/{}", file.relative_path),
                file.content,
            );
            checksum_entries.push(format!(
                r#""go/{}": "{}""#,
                file.relative_path,
                Checksum::of(file.content.as_bytes()).to_hex()
            ));
        }
        builder = builder.file(
            "/workspace/.sindri/plugins/checksums.json",
            format!("{{{}}}", checksum_entries.join(", ")),
        );
        let runtime: DummyRuntime = builder.build();
        PluginRegistry::load(&workspace(&runtime), &runtime).unwrap();
        assert!(
            runtime.written_files().is_empty(),
            "an already-valid directory should not be rewritten; wrote: {:?}",
            runtime.written_files()
        );
    }

    #[test]
    fn load_fails_when_a_files_content_does_not_match_its_recorded_checksum_before_any_other_file_is_touched() {
        let runtime: DummyRuntime = minimal_workspace()
            .file("/workspace/.sindri/plugins/go/manifest.json", "tampered")
            .file(
                "/workspace/.sindri/plugins/checksums.json",
                r#"{"go/manifest.json": "0000000000000000000000000000000000000000000000000000000000000000"}"#,
            )
            .build();
        let error: SindriError = PluginRegistry::load(&workspace(&runtime), &runtime).unwrap_err();
        assert!(
            matches!(error, SindriError::PluginModified { .. }),
            "expected PluginModified, got {error:?}"
        );
        assert!(
            runtime.written_files().is_empty(),
            "a modified manifest should fail before any file is (re)written; wrote: {:?}",
            runtime.written_files()
        );
    }
}
