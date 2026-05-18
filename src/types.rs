use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct Version(String);

impl AsRef<str> for Version {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct ModuleName(String);

impl AsRef<str> for ModuleName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceName(String);

impl AsRef<str> for WorkspaceName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct Repository(String);

impl AsRef<str> for Repository {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct BuildDirectory(PathBuf);

impl BuildDirectory {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

impl AsRef<Path> for BuildDirectory {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug)]
pub enum Language {
    Go,
    Rust,
    Java,
    Kotlin,
    Zig,
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value: String = String::deserialize(deserializer)?;
        match value.as_str() {
            "go" => Ok(Language::Go),
            "rust" => Ok(Language::Rust),
            "java" => Ok(Language::Java),
            "kotlin" => Ok(Language::Kotlin),
            "zig" => Ok(Language::Zig),
            other => Err(serde::de::Error::custom(format!(
                "unknown language `{other}`; expected one of: go, rust, java, kotlin, zig"
            ))),
        }
    }
}

#[derive(Debug)]
pub struct WorkspaceRoot(PathBuf);

impl WorkspaceRoot {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }

    pub fn workspace_file(&self) -> WorkspaceFile {
        WorkspaceFile(self.0.join("sindri.workspace"))
    }
}

impl AsRef<Path> for WorkspaceRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for WorkspaceRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

#[derive(Debug)]
pub struct WorkspaceFile(PathBuf);

impl AsRef<Path> for WorkspaceFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for WorkspaceFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

#[derive(Debug)]
pub struct BuildFile(PathBuf);

impl BuildFile {
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

impl AsRef<Path> for BuildFile {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for BuildFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}
