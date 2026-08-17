use crate::error::SindriError;
use crate::error::SindriResult;
use crate::lifecycle::EntryModule;
use crate::lifecycle::Lifecycle;
use crate::lifecycle::LifecycleName;
use crate::runtime::FileSystem;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::RelativeDirectory;
use crate::types::RelativeFile;
use crate::types::Step;
use crate::types::WorkspaceRoot;
use blake3::Hasher;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;

struct KnownLifecycle {
    file_name: &'static str,
    lifecycle_name: &'static str,
    content: &'static str,
    entry_module: EntryModule,
}

/// The lifecycles this version of Sindri manages, by fixed name — a third lifecycle, or user
/// redefinition of these two, is future work (see PLAN-LIFECYCLE.md).
const KNOWN_LIFECYCLES: [KnownLifecycle; 2] = [
    KnownLifecycle {
        file_name: "default.json",
        lifecycle_name: "default",
        content: include_str!("lifecycles/default.json"),
        entry_module: EntryModule::Required,
    },
    KnownLifecycle {
        file_name: "clean.json",
        lifecycle_name: "clean",
        content: include_str!("lifecycles/clean.json"),
        entry_module: EntryModule::Optional,
    },
];

/// A workspace's loaded lifecycles — currently always exactly `default` and `clean`, bootstrapped
/// into `.sindri/lifecycles/` from content embedded in this binary the first time either file is
/// missing, and checksum-verified against that same content on every later load. Editing these files
/// is not yet a supported operation — a mismatch is a hard error, not a silently honored edit.
#[derive(Debug)]
pub struct Lifecycles {
    lifecycles: Vec<Lifecycle>,
}

impl Lifecycles {
    /// Bootstrap-and-load `.sindri/lifecycles/` under `workspace_root`: create the directory and seed
    /// any of the known files (plus its checksum) that's missing, then verify every known file's
    /// current content against its recorded checksum before parsing it.
    pub fn load(workspace_root: &WorkspaceRoot, file_system: &impl FileSystem) -> SindriResult<Lifecycles> {
        let lifecycles_directory: AbsoluteDirectory = workspace_root.to_absolute_directory().join_directory(
            &RelativeDirectory::new(".sindri/lifecycles/").expect("a literal directory name is always well-formed"),
        );
        file_system
            .create_directories(lifecycles_directory.as_ref())
            .map_err(|source| SindriError::Io {
                path: workspace_root
                    .relativize_directory(&lifecycles_directory)
                    .as_ref()
                    .to_path_buf(),
                source,
            })?;
        let mut checksums: Checksums = Checksums::load(&lifecycles_directory, workspace_root, file_system)?;
        let mut checksums_changed: bool = false;
        let mut lifecycles: Vec<Lifecycle> = Vec::with_capacity(KNOWN_LIFECYCLES.len());
        for known in &KNOWN_LIFECYCLES {
            let file: AbsoluteFile = lifecycles_directory
                .join_file(&RelativeFile::new(known.file_name).expect("a literal file name is always well-formed"));
            let exists: bool = file_system
                .file_kind(file.as_ref())
                .map_err(|source| SindriError::Io {
                    path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                    source,
                })?
                .is_some();
            // A freshly seeded file's content is already known — `known.content`, the exact bytes just
            // written — so it is used directly rather than read back. `FileSystem` gives no guarantee
            // that a write is visible to an immediately following read (the test double in particular
            // never makes one visible to the other), and a real disk write doesn't need re-reading to
            // know what it contains either.
            let content: String = if !exists || checksums.get(known.file_name).is_none() {
                file_system
                    .write(file.as_ref(), known.content.as_bytes())
                    .map_err(|source| SindriError::Io {
                        path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                        source,
                    })?;
                checksums.insert(known.file_name, blake3_hex(known.content.as_bytes()));
                checksums_changed = true;
                known.content.to_string()
            } else {
                file_system
                    .read_to_string(file.as_ref())
                    .map_err(|source| SindriError::Io {
                        path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                        source,
                    })?
            };
            let actual_checksum: String = blake3_hex(content.as_bytes());
            if checksums.get(known.file_name) != Some(actual_checksum.as_str()) {
                return Err(SindriError::LifecycleModified {
                    path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                });
            }
            let steps: Vec<Step> = serde_json::from_str(&content).map_err(|source| SindriError::Schema {
                path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                message: source.to_string(),
            })?;
            lifecycles.push(Lifecycle::new(
                LifecycleName::new(known.lifecycle_name),
                steps,
                known.entry_module,
            ));
        }
        if checksums_changed {
            checksums.save(&lifecycles_directory, workspace_root, file_system)?;
        }
        validate_no_step_collisions(&lifecycles)?;
        Ok(Lifecycles { lifecycles })
    }

    /// The `default`/`clean` content this binary ships, built with no filesystem access — used when
    /// no workspace is found, so `--help` still has sensible content outside one. Parses the same
    /// embedded bytes [`Lifecycles::load`] seeds `.sindri/lifecycles/` with, so there is one source of
    /// truth for the built-in content.
    pub fn embedded_defaults() -> Lifecycles {
        let lifecycles: Vec<Lifecycle> = KNOWN_LIFECYCLES
            .iter()
            .map(|known: &KnownLifecycle| -> Lifecycle {
                let steps: Vec<Step> =
                    serde_json::from_str(known.content).expect("embedded lifecycle content is always valid JSON");
                Lifecycle::new(LifecycleName::new(known.lifecycle_name), steps, known.entry_module)
            })
            .collect();
        Lifecycles { lifecycles }
    }

    /// The lifecycle and step matching `name` among every non-sentinel step this collection carries.
    pub fn find_step(&self, name: &str) -> Option<(&Lifecycle, &Step)> {
        self.lifecycles.iter().find_map(|lifecycle: &Lifecycle| {
            lifecycle
                .steps()
                .iter()
                .find(|step: &&Step| step.as_ref() == name && !is_sentinel(step))
                .map(|step: &Step| (lifecycle, step))
        })
    }

    /// Consumes this collection, handing back the owned lifecycle and step matching `name` — used
    /// once dispatch has resolved which step to run, since [`Lifecycle::run_step`] takes `self` by
    /// value.
    pub fn into_step(self, name: &str) -> Option<(Lifecycle, Step)> {
        for lifecycle in self.lifecycles {
            let matched_step: Option<Step> = lifecycle
                .steps()
                .iter()
                .find(|step: &&Step| step.as_ref() == name && !is_sentinel(step))
                .cloned();
            if let Some(step) = matched_step {
                return Some((lifecycle, step));
            }
        }
        None
    }

    /// The `default` lifecycle among these — always present, since `default` is one of the two
    /// lifecycles Sindri loads. Used by the `lifecycle` subcommand's listing.
    pub fn default_lifecycle(&self) -> &Lifecycle {
        self.lifecycles
            .iter()
            .find(|lifecycle: &&Lifecycle| lifecycle.name() == &LifecycleName::new("default"))
            .expect("`default` is always one of the loaded lifecycles")
    }

    /// Every non-sentinel step across every loaded lifecycle, in a stable order — what the CLI uses
    /// to build subcommands and help text.
    pub fn runnable_steps(&self) -> impl Iterator<Item = (&Lifecycle, &Step)> {
        self.lifecycles.iter().flat_map(|lifecycle: &Lifecycle| {
            lifecycle
                .steps()
                .iter()
                .filter(|step: &&Step| !is_sentinel(step))
                .map(move |step: &Step| (lifecycle, step))
        })
    }
}

fn is_sentinel(step: &Step) -> bool {
    step.as_ref() == "start" || step.as_ref() == "end"
}

/// No two lifecycles may bind the same non-sentinel step name — the CLI resolves a step name to a
/// single lifecycle to run, so a collision would be ambiguous. Not reachable with just `default` and
/// `clean` today (their step sets are disjoint), but asserted as an explicit invariant rather than an
/// assumption, since it stops holding for free once a third lifecycle is added.
fn validate_no_step_collisions(lifecycles: &[Lifecycle]) -> SindriResult<()> {
    let mut owners: HashMap<&str, &LifecycleName> = HashMap::new();
    for lifecycle in lifecycles {
        for step in lifecycle.steps() {
            if is_sentinel(step) {
                continue;
            }
            if let Some(first) = owners.get(step.as_ref()) {
                return Err(SindriError::LifecycleStepCollision {
                    step: step.clone(),
                    first: (*first).clone(),
                    second: lifecycle.name().clone(),
                });
            }
            owners.insert(step.as_ref(), lifecycle.name());
        }
    }
    Ok(())
}

fn blake3_hex(bytes: &[u8]) -> String {
    let mut hasher: Hasher = Hasher::new();
    hasher.update(bytes);
    hasher.finalize().to_hex().to_string()
}

/// The checksum manifest at `.sindri/lifecycles/checksums.json` — recorded whenever a known lifecycle
/// file is (re)seeded, and checked on every later load, since Sindri doesn't yet support editing these
/// files: a mismatch means the file diverged from what Sindri itself wrote.
#[derive(Default, Serialize, Deserialize)]
struct Checksums(BTreeMap<String, String>);

impl Checksums {
    fn load(
        directory: &AbsoluteDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> SindriResult<Checksums> {
        let file: AbsoluteFile = checksums_file(directory);
        let exists: bool = file_system
            .file_kind(file.as_ref())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                source,
            })?
            .is_some();
        if !exists {
            return Ok(Checksums::default());
        }
        let content: String = file_system
            .read_to_string(file.as_ref())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                source,
            })?;
        serde_json::from_str(&content).map_err(|source| SindriError::Schema {
            path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
            message: source.to_string(),
        })
    }

    fn save(
        &self,
        directory: &AbsoluteDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> SindriResult<()> {
        let file: AbsoluteFile = checksums_file(directory);
        let content: String =
            serde_json::to_string_pretty(self).expect("checksum manifest serialization is infallible");
        file_system
            .write(file.as_ref(), content.as_bytes())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relativize_file(&file).as_ref().to_path_buf(),
                source,
            })
    }

    fn get(&self, file_name: &str) -> Option<&str> {
        self.0.get(file_name).map(String::as_str)
    }

    fn insert(&mut self, file_name: &str, checksum: String) {
        self.0.insert(file_name.to_string(), checksum);
    }
}

fn checksums_file(directory: &AbsoluteDirectory) -> AbsoluteFile {
    directory.join_file(&RelativeFile::new("checksums.json").expect("a literal file name is always well-formed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::types::AbsoluteDirectory;
    use std::io::ErrorKind;
    use std::path::PathBuf;

    fn workspace_root() -> WorkspaceRoot {
        WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")))
    }

    #[test]
    fn load_seeds_both_files_and_a_checksum_manifest_when_missing() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let lifecycles: Lifecycles = Lifecycles::load(&workspace_root(), &runtime).unwrap();
        assert_eq!(
            runtime
                .written_file("/workspace/.sindri/lifecycles/default.json")
                .unwrap(),
            KNOWN_LIFECYCLES[0].content.as_bytes()
        );
        assert_eq!(
            runtime
                .written_file("/workspace/.sindri/lifecycles/clean.json")
                .unwrap(),
            KNOWN_LIFECYCLES[1].content.as_bytes()
        );
        assert!(
            runtime
                .written_file("/workspace/.sindri/lifecycles/checksums.json")
                .is_some(),
            "expected a checksum manifest to be written"
        );
        assert_eq!(lifecycles.runnable_steps().count(), 11);
    }

    #[test]
    fn load_recreates_only_the_missing_file_and_leaves_a_valid_sibling_untouched() {
        let default_checksum: String = blake3_hex(KNOWN_LIFECYCLES[0].content.as_bytes());
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(
                "/workspace/.sindri/lifecycles/default.json",
                KNOWN_LIFECYCLES[0].content,
            )
            .file(
                "/workspace/.sindri/lifecycles/checksums.json",
                format!(r#"{{"default.json": "{default_checksum}"}}"#),
            )
            .build();
        let lifecycles: Lifecycles = Lifecycles::load(&workspace_root(), &runtime).unwrap();
        assert_eq!(
            runtime
                .written_file("/workspace/.sindri/lifecycles/clean.json")
                .unwrap(),
            KNOWN_LIFECYCLES[1].content.as_bytes()
        );
        assert!(
            lifecycles.find_step("clean").is_some(),
            "the recreated clean.json should still load correctly"
        );
        assert!(
            runtime
                .written_file("/workspace/.sindri/lifecycles/default.json")
                .is_none(),
            "the already-valid default.json should not have been rewritten"
        );
    }

    #[test]
    fn load_reads_a_pre_seeded_checksum_matching_directory_without_rewriting_it() {
        let default_checksum: String = blake3_hex(KNOWN_LIFECYCLES[0].content.as_bytes());
        let clean_checksum: String = blake3_hex(KNOWN_LIFECYCLES[1].content.as_bytes());
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file(
                "/workspace/.sindri/lifecycles/default.json",
                KNOWN_LIFECYCLES[0].content,
            )
            .file("/workspace/.sindri/lifecycles/clean.json", KNOWN_LIFECYCLES[1].content)
            .file(
                "/workspace/.sindri/lifecycles/checksums.json",
                format!(r#"{{"default.json": "{default_checksum}", "clean.json": "{clean_checksum}"}}"#),
            )
            .build();
        Lifecycles::load(&workspace_root(), &runtime).unwrap();
        assert!(
            runtime.written_files().is_empty(),
            "an already-valid directory should not be rewritten; wrote: {:?}",
            runtime.written_files()
        );
    }

    #[test]
    fn load_fails_when_a_files_content_does_not_match_its_recorded_checksum() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/.sindri/lifecycles/default.json", "[\"tampered\"]")
            .file(
                "/workspace/.sindri/lifecycles/checksums.json",
                r#"{"default.json": "0000000000000000000000000000000000000000000000000000000000000000"}"#,
            )
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(
            matches!(error, SindriError::LifecycleModified { .. }),
            "expected LifecycleModified, got {error:?}"
        );
    }

    #[test]
    fn load_fails_when_creating_the_lifecycles_directory_errors() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .write_error("/workspace/.sindri/lifecycles/", ErrorKind::PermissionDenied)
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }), "expected Io, got {error:?}");
    }

    #[test]
    fn load_fails_when_seeding_a_missing_known_file_errors() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .write_error(
                "/workspace/.sindri/lifecycles/default.json",
                ErrorKind::PermissionDenied,
            )
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }), "expected Io, got {error:?}");
    }

    #[test]
    fn load_fails_when_saving_the_checksum_manifest_errors() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .write_error(
                "/workspace/.sindri/lifecycles/checksums.json",
                ErrorKind::PermissionDenied,
            )
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }), "expected Io, got {error:?}");
    }

    #[test]
    fn load_fails_when_checking_a_known_files_kind_errors() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .error(
                "/workspace/.sindri/lifecycles/default.json",
                ErrorKind::PermissionDenied,
            )
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }), "expected Io, got {error:?}");
    }

    #[test]
    fn load_fails_when_checking_the_checksum_manifests_kind_errors() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .error(
                "/workspace/.sindri/lifecycles/checksums.json",
                ErrorKind::PermissionDenied,
            )
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }), "expected Io, got {error:?}");
    }

    #[test]
    fn load_fails_when_an_existing_files_content_is_not_valid_utf8() {
        let invalid_utf8: Vec<u8> = vec![0xFF, 0xFE, 0xFD];
        let checksum: String = blake3_hex(&invalid_utf8);
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/.sindri/lifecycles/default.json", invalid_utf8)
            .file(
                "/workspace/.sindri/lifecycles/checksums.json",
                format!(r#"{{"default.json": "{checksum}"}}"#),
            )
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }), "expected Io, got {error:?}");
    }

    #[test]
    fn load_fails_when_an_existing_files_content_is_valid_utf8_but_not_valid_json() {
        let content: &str = "not a json array";
        let checksum: String = blake3_hex(content.as_bytes());
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/.sindri/lifecycles/default.json", content)
            .file(
                "/workspace/.sindri/lifecycles/checksums.json",
                format!(r#"{{"default.json": "{checksum}"}}"#),
            )
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(
            matches!(error, SindriError::Schema { .. }),
            "expected Schema, got {error:?}"
        );
    }

    #[test]
    fn load_fails_when_the_checksum_manifest_itself_is_not_valid_json() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/.sindri/lifecycles/checksums.json", "not json")
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(
            matches!(error, SindriError::Schema { .. }),
            "expected Schema, got {error:?}"
        );
    }

    #[test]
    fn load_fails_when_the_checksum_manifests_content_is_not_valid_utf8() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/workspace/.sindri/lifecycles/checksums.json", vec![0xFF, 0xFE, 0xFD])
            .build();
        let error: SindriError = Lifecycles::load(&workspace_root(), &runtime).unwrap_err();
        assert!(matches!(error, SindriError::Io { .. }), "expected Io, got {error:?}");
    }

    #[test]
    fn embedded_defaults_matches_what_load_would_seed_on_disk() {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let loaded: Lifecycles = Lifecycles::load(&workspace_root(), &runtime).unwrap();
        let embedded: Lifecycles = Lifecycles::embedded_defaults();
        let names = |lifecycles: &Lifecycles| -> Vec<String> {
            lifecycles
                .runnable_steps()
                .map(|(_, step): (&Lifecycle, &Step)| step.to_string())
                .collect()
        };
        assert_eq!(names(&loaded), names(&embedded));
    }

    #[test]
    fn find_step_resolves_a_step_to_its_owning_lifecycle() {
        let lifecycles: Lifecycles = Lifecycles::embedded_defaults();
        let (lifecycle, step): (&Lifecycle, &Step) = lifecycles.find_step("clean").unwrap();
        assert_eq!(lifecycle.name().to_string(), "clean");
        assert_eq!(step.to_string(), "clean");
        assert!(lifecycles.find_step("does-not-exist").is_none());
        assert!(
            lifecycles.find_step("start").is_none(),
            "sentinels are not runnable steps"
        );
    }

    #[test]
    fn into_step_hands_back_the_owned_lifecycle_and_step_matching_a_name() {
        let (lifecycle, step): (Lifecycle, Step) = Lifecycles::embedded_defaults().into_step("clean").unwrap();
        assert_eq!(lifecycle.name().to_string(), "clean");
        assert_eq!(step.to_string(), "clean");
    }

    #[test]
    fn into_step_returns_none_for_an_unknown_name() {
        assert!(Lifecycles::embedded_defaults().into_step("does-not-exist").is_none());
    }

    #[test]
    fn default_lifecycle_resolves_the_default_lifecycle() {
        assert_eq!(
            Lifecycles::embedded_defaults().default_lifecycle().name().to_string(),
            "default"
        );
    }

    #[test]
    fn validate_no_step_collisions_rejects_two_lifecycles_sharing_a_step() {
        let lifecycles: Vec<Lifecycle> = vec![
            Lifecycle::new(
                LifecycleName::new("a"),
                vec![Step::new("compile")],
                EntryModule::Required,
            ),
            Lifecycle::new(
                LifecycleName::new("b"),
                vec![Step::new("compile")],
                EntryModule::Required,
            ),
        ];
        let error: SindriError = validate_no_step_collisions(&lifecycles).unwrap_err();
        assert!(
            matches!(error, SindriError::LifecycleStepCollision { .. }),
            "expected LifecycleStepCollision, got {error:?}"
        );
    }

    #[test]
    fn validate_no_step_collisions_accepts_disjoint_lifecycles() {
        let lifecycles: Vec<Lifecycle> = vec![
            Lifecycle::new(
                LifecycleName::new("a"),
                vec![Step::new("compile")],
                EntryModule::Required,
            ),
            Lifecycle::new(LifecycleName::new("b"), vec![Step::new("clean")], EntryModule::Optional),
        ];
        assert!(validate_no_step_collisions(&lifecycles).is_ok());
    }
}
