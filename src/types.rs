use serde::Deserialize;
use smol_str::SmolStr;
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Step(SmolStr);

impl Step {
    pub fn new(name: impl Into<SmolStr>) -> Self {
        Self(name.into())
    }
}

impl AsRef<str> for Step {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Step {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct Version(SmolStr);

impl AsRef<str> for Version {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct ModuleName(SmolStr);

impl AsRef<str> for ModuleName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceName(SmolStr);

impl AsRef<str> for WorkspaceName {
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
}

impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value: SmolStr = SmolStr::deserialize(deserializer)?;
        match value.as_str() {
            "go" => Ok(Language::Go),
            other => Err(serde::de::Error::custom(format!(
                "unknown language `{other}`; expected one of: go"
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
