use std::env;
use std::fs::read_to_string;
use std::path::PathBuf;

/// Reads the `nickel-lang-core` version Cargo actually resolved out of `Cargo.lock`, and exposes it
/// to the crate as `env!("NICKEL_LANG_CORE_VERSION")`. `Task::definition_hash` folds it into the
/// Nickel version salt, so it must always match the pinned dependency exactly — reading it here
/// rather than hand-maintaining a constant means it can never silently drift out of sync.
fn main() {
    let manifest_directory: PathBuf =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR for build scripts"));
    let lock_file: PathBuf = manifest_directory.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_file.display());
    let lock_file_contents: String =
        read_to_string(&lock_file).unwrap_or_else(|error| panic!("could not read {}: {error}", lock_file.display()));
    let version: &str = nickel_lang_core_version(&lock_file_contents).unwrap_or_else(|| {
        panic!(
            "{} has no `nickel-lang-core` package entry; is it still a dependency?",
            lock_file.display()
        )
    });
    println!("cargo:rustc-env=NICKEL_LANG_CORE_VERSION={version}");
}

/// Find the `version` field of the `[[package]] name = "nickel-lang-core"` entry in a `Cargo.lock`
/// file's raw text. A hand-rolled scan rather than a TOML parser, since `Cargo.lock`'s package
/// entries are a fixed, simple shape and a build script is not worth an extra dependency for this.
fn nickel_lang_core_version(lock_file_contents: &str) -> Option<&str> {
    let mut lines = lock_file_contents.lines();
    while let Some(line) = lines.next() {
        if line.trim() == "name = \"nickel-lang-core\"" {
            let version_line: &str = lines.next()?;
            return version_line.trim().strip_prefix("version = \"")?.strip_suffix('"');
        }
    }
    None
}
