use crate::glob::GlobPatterns;
use crate::types::Command;
use blake3::Hasher;
use serde::Deserialize;
use serde::Serialize;

/// A field delimiter folded into a hash between distinct pieces, so that concatenating two different
/// field splittings can never produce the same byte stream (e.g. `["a", "bc"]` vs `["ab", "c"]`).
const FIELD_SEPARATOR: [u8; 1] = [0];

/// The blake3 digest of a single file's content, after normalizing line endings. Two checkouts of
/// the same file that differ only in CRLF vs LF therefore hash identically. Distinct from the other
/// hash newtypes so a file digest can never be compared against a set or declaration digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileHash([u8; 32]);

impl FileHash {
    /// Hash a file's raw bytes, normalizing CRLF to LF in a single pass: a `\r` is dropped only when
    /// it immediately precedes a `\n`, so a lone `\r` is preserved and no second buffer is allocated.
    pub fn of_bytes(bytes: &[u8]) -> FileHash {
        let mut hasher: Hasher = Hasher::new();
        let mut run_start: usize = 0;
        let mut index: usize = 0;
        while index < bytes.len() {
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                hasher.update(&bytes[run_start..index]);
                run_start = index + 1;
            }
            index += 1;
        }
        hasher.update(&bytes[run_start..]);
        FileHash(*hasher.finalize().as_bytes())
    }

    fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// The blake3 digest of a whole set of files — each contributing its workspace-relative path and its
/// [`FileHash`]. The set is sorted by path before folding, so the digest is independent of the order
/// the files were discovered in; adding, removing, or changing any file changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileSetHash([u8; 32]);

impl FileSetHash {
    pub fn of(files: impl IntoIterator<Item = (String, FileHash)>) -> FileSetHash {
        let mut files: Vec<(String, FileHash)> = files.into_iter().collect();
        files.sort_by(|first: &(String, FileHash), second: &(String, FileHash)| first.0.cmp(&second.0));
        let mut hasher: Hasher = Hasher::new();
        for (path, file_hash) in &files {
            hasher.update(path.as_bytes());
            hasher.update(&FIELD_SEPARATOR);
            hasher.update(file_hash.as_bytes());
        }
        FileSetHash(*hasher.finalize().as_bytes())
    }
}

/// The blake3 digest of a task's declaration — its command and its input and output glob patterns.
/// Folding it into a task's state means a plugin update that changes the command or globs forces the
/// task to re-run even when no source file changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeclarationHash([u8; 32]);

impl DeclarationHash {
    pub fn of(command: &Command, inputs: &GlobPatterns, outputs: &GlobPatterns) -> DeclarationHash {
        let mut hasher: Hasher = Hasher::new();
        update_field(&mut hasher, command.program());
        for argument in command.arguments() {
            update_field(&mut hasher, argument.as_str());
        }
        for patterns in [inputs, outputs] {
            for glob in patterns.includes().iter().chain(patterns.excludes()) {
                update_field(&mut hasher, glob.as_str());
            }
        }
        DeclarationHash(*hasher.finalize().as_bytes())
    }
}

fn update_field(hasher: &mut Hasher, field: &str) {
    hasher.update(field.as_bytes());
    hasher.update(&FIELD_SEPARATOR);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glob::Glob;

    fn file_hash(bytes: &[u8]) -> FileHash {
        FileHash::of_bytes(bytes)
    }

    #[test]
    fn crlf_and_lf_line_endings_hash_identically() {
        assert_eq!(file_hash(b"first\r\nsecond\r\n"), file_hash(b"first\nsecond\n"));
    }

    #[test]
    fn a_lone_carriage_return_is_preserved() {
        assert_ne!(file_hash(b"bare\rreturn"), file_hash(b"bare\nreturn"));
    }

    #[test]
    fn changing_a_byte_changes_the_file_hash() {
        assert_ne!(file_hash(b"package main"), file_hash(b"package mail"));
    }

    #[test]
    fn file_set_hash_is_independent_of_order() {
        let first: FileSetHash = FileSetHash::of([
            ("a.go".to_string(), file_hash(b"alpha")),
            ("b.go".to_string(), file_hash(b"beta")),
        ]);
        let second: FileSetHash = FileSetHash::of([
            ("b.go".to_string(), file_hash(b"beta")),
            ("a.go".to_string(), file_hash(b"alpha")),
        ]);
        assert_eq!(first, second);
    }

    #[test]
    fn changing_a_file_changes_the_set_hash() {
        let original: FileSetHash = FileSetHash::of([("a.go".to_string(), file_hash(b"alpha"))]);
        let edited: FileSetHash = FileSetHash::of([("a.go".to_string(), file_hash(b"alpha!"))]);
        assert_ne!(original, edited);
    }

    #[test]
    fn changing_the_command_changes_the_declaration_hash() {
        let inputs: GlobPatterns = GlobPatterns::new(vec![Glob::new("**/*.go")], vec![]);
        let outputs: GlobPatterns = GlobPatterns::new(vec![], vec![]);
        let build: DeclarationHash = DeclarationHash::of(&Command::new("go", ["build"]), &inputs, &outputs);
        let vet: DeclarationHash = DeclarationHash::of(&Command::new("go", ["vet"]), &inputs, &outputs);
        assert_ne!(build, vet);
    }
}
