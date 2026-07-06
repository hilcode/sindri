use crate::glob::Glob;
use crate::glob::GlobPatterns;
use crate::module::ArtifactType;
use crate::types::AbsoluteFile;
use crate::types::Command;
use crate::types::Step;
use serde::Deserialize;
use smol_str::SmolStr;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskName(SmolStr);

impl TaskName {
    pub fn new(name: impl Into<SmolStr>) -> Self {
        Self(name.into())
    }
}

impl AsRef<str> for TaskName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl Display for TaskName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(transparent)]
pub struct PluginName(SmolStr);

impl PluginName {
    pub fn new(name: impl Into<SmolStr>) -> Self {
        Self(name.into())
    }
}

impl Display for PluginName {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    name: TaskName,
    step: Step,
    command: Command,
    inputs: GlobPatterns,
    outputs: GlobPatterns,
}

impl Task {
    pub fn new(name: TaskName, step: Step, command: Command, inputs: GlobPatterns, outputs: GlobPatterns) -> Task {
        Task {
            name,
            step,
            command,
            inputs,
            outputs,
        }
    }

    pub fn name(&self) -> &TaskName {
        &self.name
    }

    pub fn step(&self) -> &Step {
        &self.step
    }

    pub fn command(&self) -> &Command {
        &self.command
    }

    pub fn inputs(&self) -> &GlobPatterns {
        &self.inputs
    }

    pub fn outputs(&self) -> &GlobPatterns {
        &self.outputs
    }
}

#[derive(Debug)]
pub struct Plugin {
    name: PluginName,
    tasks: Vec<Task>,
}

impl Plugin {
    pub fn new(name: PluginName, tasks: Vec<Task>) -> Plugin {
        Plugin { name, tasks }
    }

    pub fn name(&self) -> &PluginName {
        &self.name
    }

    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }
}

/// A platform-independent superset of every file type the Go toolchain might compile — all Go and
/// C-family sources cgo can pull in, plus assembly and the module manifests. Over-inclusion only
/// ever costs a spurious rebuild; it never misses an input. Per-target precision via `go list` is
/// future work.
fn go_source_superset() -> Vec<Glob> {
    vec![
        Glob::new("**/*.{go,c,h,cc,cpp,cxx,hh,hpp,hxx,m,s,S}"),
        Glob::new("go.mod"),
        Glob::new("go.sum"),
    ]
}

/// The go-compile command for a module of the given type. An executable is linked into a runnable
/// binary written to the tracked output directory, so its build is `go build -o {output}/ ./...` and
/// the binary becomes a tracked output. A library has no runnable binary — `go build` on it only
/// type-checks and compiles its packages into Go's own cache — so it is `go build ./...`, which also
/// avoids the `-o` form's "no main packages to build" error on a module with no executable.
fn go_compile_command(artifact_type: &ArtifactType) -> Command {
    match artifact_type {
        ArtifactType::Executable => Command::new("go", ["build", "-o", "{output}/", "./..."]),
        _ => Command::new("go", ["build", "./..."]),
    }
}

/// Point a module-aware `go` invocation at Sindri's generated workspace file via `GOWORK`, so it
/// resolves imports of sibling modules to their local directories. `go_work` is absent only for the
/// `lifecycle` listing, which spawns nothing; `gofmt` needs no workspace and is never wrapped.
fn with_go_work(command: Command, go_work: Option<&AbsoluteFile>) -> Command {
    match go_work {
        Some(go_work) => command.with_environment_variable("GOWORK", go_work.as_ref().to_string_lossy().into_owned()),
        None => command,
    }
}

pub fn go_plugin(artifact_type: &ArtifactType, go_work: Option<&AbsoluteFile>) -> Plugin {
    Plugin {
        name: PluginName::new("sindri-go"),
        tasks: vec![
            Task {
                name: TaskName::new("go-format"),
                step: Step::new("format"),
                command: Command::new("gofmt", ["-l", "."]),
                inputs: GlobPatterns::new(vec![Glob::new("**/*.go")], vec![]),
                outputs: GlobPatterns::new(vec![], vec![]),
            },
            Task {
                name: TaskName::new("go-compile"),
                step: Step::new("compile"),
                command: with_go_work(go_compile_command(artifact_type), go_work),
                inputs: GlobPatterns::new(go_source_superset(), vec![]),
                outputs: GlobPatterns::new(vec![Glob::new("**/*")], vec![]),
            },
            Task {
                name: TaskName::new("go-test"),
                step: Step::new("test"),
                command: with_go_work(Command::new("go", ["test", "./..."]), go_work),
                inputs: GlobPatterns::new(go_source_superset(), vec![]),
                outputs: GlobPatterns::new(vec![], vec![]),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn go_compile_task(artifact_type: &ArtifactType) -> Task {
        go_plugin(artifact_type, None)
            .tasks
            .into_iter()
            .find(|task: &Task| task.name() == &TaskName::new("go-compile"))
            .expect("the go plugin contributes a go-compile task")
    }

    #[test]
    fn go_plugin_contributes_to_correct_lifecycle_steps() {
        let plugin: Plugin = go_plugin(&ArtifactType::Executable, None);
        let tasks_in = |step_name: &str| -> Vec<&Task> {
            let step: Step = Step::new(step_name);
            plugin.tasks.iter().filter(|task| task.step() == &step).collect()
        };
        let format_tasks: Vec<&Task> = tasks_in("format");
        let compile_tasks: Vec<&Task> = tasks_in("compile");
        let test_tasks: Vec<&Task> = tasks_in("test");
        assert_eq!(format_tasks.len(), 1);
        assert_eq!(format_tasks[0].name(), &TaskName::new("go-format"));
        assert_eq!(compile_tasks.len(), 1);
        assert_eq!(compile_tasks[0].name(), &TaskName::new("go-compile"));
        assert_eq!(test_tasks.len(), 1);
        assert_eq!(test_tasks[0].name(), &TaskName::new("go-test"));
    }

    #[test]
    fn executable_go_compile_writes_a_binary_to_the_output_directory() {
        // An executable must emit its binary into the tracked output directory, so its command
        // carries the `-o {output}/` form.
        let command: String = go_compile_task(&ArtifactType::Executable).command().to_string();
        assert_eq!(command, "go build -o {output}/ ./...");
    }

    #[test]
    fn library_go_compile_omits_the_output_binary() {
        // A library has no runnable binary; `go build -o {output}/ ./...` would fail with "no main
        // packages to build", so a library compiles with the plain `./...` form.
        let command: String = go_compile_task(&ArtifactType::Library).command().to_string();
        assert_eq!(command, "go build ./...");
    }

    #[test]
    fn go_compile_carries_the_gowork_environment_when_a_workspace_file_is_given() {
        let go_work: AbsoluteFile = AbsoluteFile::new(PathBuf::from("/workspace/.target/go.work"));
        let plugin: Plugin = go_plugin(&ArtifactType::Executable, Some(&go_work));
        let go_compile: &Task = plugin
            .tasks
            .iter()
            .find(|task: &&Task| task.name() == &TaskName::new("go-compile"))
            .unwrap();
        assert_eq!(
            go_compile.command().environment(),
            &[(SmolStr::new("GOWORK"), SmolStr::new("/workspace/.target/go.work"))]
        );
        // The listing form (no workspace file) sets no environment.
        assert!(
            go_compile_task(&ArtifactType::Executable)
                .command()
                .environment()
                .is_empty()
        );
    }
}
