use crate::error::SindriError;
use crate::error::SindriResult;
use crate::runtime::FileSystem;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::RelativeFile;
use crate::types::WorkspaceRoot;
use blake3::Hash;
use blake3::Hasher;
use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::Error as DeserializeError;
use std::collections::BTreeMap;

/// A blake3 digest of a known file's content. Serializes as its hex encoding, so
/// `checksums.json` stays a plain `{ "<file>": "<hex digest>" }` map rather than an array of
/// byte values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Checksum(Hash);

impl Checksum {
    pub(crate) fn of(bytes: &[u8]) -> Checksum {
        let mut hasher: Hasher = Hasher::new();
        hasher.update(bytes);
        Checksum(hasher.finalize())
    }

    /// This checksum's hex encoding, as it appears in `checksums.json` — test-only, for building
    /// fixture checksum-manifest content.
    #[cfg(test)]
    pub(crate) fn to_hex(self) -> String {
        self.0.to_hex().to_string()
    }
}

impl Serialize for Checksum {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0.to_hex())
    }
}

impl<'deserialize> Deserialize<'deserialize> for Checksum {
    fn deserialize<D: Deserializer<'deserialize>>(deserializer: D) -> Result<Self, D::Error> {
        let hex: String = String::deserialize(deserializer)?;
        Hash::from_hex(&hex).map(Checksum).map_err(DeserializeError::custom)
    }
}

/// The checksum manifest recorded alongside a directory of Sindri-managed embedded content
/// (`.sindri/lifecycles/`, `.sindri/plugins/`): recorded whenever a known file is (re)seeded, and
/// checked against that file's actual content on every later load, since editing this content isn't
/// supported — a mismatch means the file diverged from what Sindri itself wrote. Keyed by each known
/// file's path relative to the directory the manifest itself lives in.
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Checksums(BTreeMap<RelativeFile, Checksum>);

impl Checksums {
    pub(crate) fn load(
        directory: &AbsoluteDirectory,
        workspace_root: &WorkspaceRoot,
        file_system: &impl FileSystem,
    ) -> SindriResult<Checksums> {
        let file: AbsoluteFile = checksums_file(directory);
        let exists: bool = file_system
            .file_kind(file.as_ref())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relative_path_buf(&file),
                source,
            })?
            .is_some();
        if !exists {
            return Ok(Checksums::default());
        }
        let content: String = file_system
            .read_to_string(file.as_ref())
            .map_err(|source| SindriError::Io {
                path: workspace_root.relative_path_buf(&file),
                source,
            })?;
        serde_json::from_str(&content).map_err(|source| SindriError::Schema {
            path: workspace_root.relative_path_buf(&file),
            message: source.to_string(),
        })
    }

    pub(crate) fn save(
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
                path: workspace_root.relative_path_buf(&file),
                source,
            })
    }

    pub(crate) fn get(&self, file: &RelativeFile) -> Option<&Checksum> {
        self.0.get(file)
    }

    pub(crate) fn insert(&mut self, file: RelativeFile, checksum: Checksum) {
        self.0.insert(file, checksum);
    }
}

fn checksums_file(directory: &AbsoluteDirectory) -> AbsoluteFile {
    directory.join_file(&RelativeFile::new("checksums.json").expect("a literal file name is always well-formed"))
}
