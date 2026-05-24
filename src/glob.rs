use globset::Glob as GlobMatcher;
use globset::GlobSet;
use globset::GlobSetBuilder;
use smol_str::SmolStr;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;
use std::path::Path;

/// A single glob pattern (e.g. `**/*.go`). Stored as text; compiled into a matcher only when a file
/// set is actually expanded.
#[derive(Clone, Debug)]
pub struct Glob(SmolStr);

impl Glob {
    pub fn new(pattern: impl Into<SmolStr>) -> Self {
        Self(pattern.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for Glob {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

/// A set of include globs minus a set of exclude globs. A file is selected iff it matches at least
/// one include **and** matches no exclude; excludes may target whole directory subtrees. The MVP
/// only populates includes, but the exclude list exists from the start so subtraction is supported
/// without a later type change.
#[derive(Clone, Debug)]
pub struct GlobPatterns {
    includes: Vec<Glob>,
    excludes: Vec<Glob>,
}

impl GlobPatterns {
    pub fn new(includes: Vec<Glob>, excludes: Vec<Glob>) -> GlobPatterns {
        GlobPatterns { includes, excludes }
    }

    pub fn includes(&self) -> &[Glob] {
        &self.includes
    }

    pub fn excludes(&self) -> &[Glob] {
        &self.excludes
    }

    /// Compile the include and exclude patterns into [`CompiledGlobs`] for in-memory matching. Used
    /// where there is no real directory tree to walk (the test runtime); the on-disk runtime instead
    /// drives the `ignore` walker, which prunes excluded subtrees during traversal.
    pub fn compiled(&self) -> Result<CompiledGlobs, globset::Error> {
        Ok(CompiledGlobs {
            includes: build_glob_set(&self.includes)?,
            excludes: build_glob_set(&self.excludes)?,
        })
    }
}

fn build_glob_set(globs: &[Glob]) -> Result<GlobSet, globset::Error> {
    let mut builder: GlobSetBuilder = GlobSetBuilder::new();
    for glob in globs {
        builder.add(GlobMatcher::new(glob.as_str())?);
    }
    builder.build()
}

/// Compiled [`GlobPatterns`]: an include matcher and an exclude matcher, applied to paths expressed
/// relative to the directory the patterns are anchored in.
pub struct CompiledGlobs {
    includes: GlobSet,
    excludes: GlobSet,
}

impl CompiledGlobs {
    pub fn is_match(&self, relative_path: &Path) -> bool {
        self.includes.is_match(relative_path) && !self.excludes.is_match(relative_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DummyRuntime;
    use crate::runtime::FileSystem;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn select(runtime: &DummyRuntime, base: &str, patterns: GlobPatterns) -> BTreeSet<PathBuf> {
        runtime
            .matching_files(Path::new(base), &patterns)
            .unwrap()
            .into_iter()
            .collect()
    }

    fn includes(patterns: &[&str]) -> GlobPatterns {
        GlobPatterns::new(patterns.iter().map(|pattern| Glob::new(*pattern)).collect(), vec![])
    }

    #[test]
    fn include_matches_files_under_its_prefix_and_none_outside() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/module/src/main.go", "")
            .file("/module/src/inner/helper.go", "")
            .file("/module/other/elsewhere.go", "")
            .file("/module/top.go", "")
            .build();
        let matched: BTreeSet<PathBuf> = select(&runtime, "/module", includes(&["src/**/*.go"]));
        assert_eq!(
            matched,
            BTreeSet::from([
                PathBuf::from("/module/src/inner/helper.go"),
                PathBuf::from("/module/src/main.go"),
            ])
        );
    }

    #[test]
    fn exclude_pattern_removes_a_matching_file() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/module/keep.go", "")
            .file("/module/vendor/drop.go", "")
            .build();
        let patterns: GlobPatterns = GlobPatterns::new(vec![Glob::new("**/*.go")], vec![Glob::new("vendor/**")]);
        let matched: BTreeSet<PathBuf> = select(&runtime, "/module", patterns);
        assert_eq!(matched, BTreeSet::from([PathBuf::from("/module/keep.go")]));
    }

    #[test]
    fn go_superset_tracks_cgo_c_and_h_files() {
        let runtime: DummyRuntime = DummyRuntime::builder()
            .file("/module/main.go", "")
            .file("/module/bridge.c", "")
            .file("/module/bridge.h", "")
            .file("/module/README.md", "")
            .build();
        let matched: BTreeSet<PathBuf> = select(
            &runtime,
            "/module",
            includes(&["**/*.{go,c,h,cc,cpp,cxx,hh,hpp,hxx,m,s,S}"]),
        );
        assert_eq!(
            matched,
            BTreeSet::from([
                PathBuf::from("/module/bridge.c"),
                PathBuf::from("/module/bridge.h"),
                PathBuf::from("/module/main.go"),
            ])
        );
    }
}
