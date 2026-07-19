use crate::file_set::FileSetPattern;
use crate::module::ArtifactType;
use crate::parameter::ParameterDeclarations;
use crate::script::Script;
use crate::task::DeclaredTaskInput;
use crate::task::ManagedTaskInput;
use crate::task::Task;
use crate::task::TaskName;
use crate::task::TaskOutput;
use crate::types::Step;

/// A platform-independent superset of every file type the Go toolchain might compile — all Go and
/// C-family sources cgo can pull in, plus assembly and the module manifests. Over-inclusion only
/// ever costs a spurious rebuild; it never misses an input. Per-target precision via `go list` is
/// future work.
fn go_source_superset() -> FileSetPattern {
    FileSetPattern::new(["**/*.{go,c,h,cc,cpp,cxx,hh,hpp,hxx,m,s,S}", "go.mod", "go.sum"])
}

/// The built-in Go plugin's tasks, contributed as shipped Nickel `Script`s rather than a hand-built
/// `Command`. No managed input yet (a generated `go.work` participating in dirtiness is a later
/// phase) and no declared parameters yet (a `mode` binding is a later phase).
pub struct GoPlugin;

impl GoPlugin {
    /// The `go-format` / `go-compile` / `go-test` tasks, each paired with the lifecycle step it is
    /// bound to. `go-compile`'s script depends on `artifact_type`: an executable links a runnable
    /// binary into the tracked output directory, so its build is `go build -o <output-directory>/
    /// ./...` and the binary becomes a tracked output; a library has no runnable binary — `go build`
    /// on it only type-checks and compiles its packages into Go's own cache — so it is
    /// `go build ./...`, which also avoids the `-o` form's "no main packages to build" error on a
    /// module with no executable.
    pub fn tasks(artifact_type: &ArtifactType) -> Vec<(Task, Step)> {
        let compile_script: Script = match artifact_type {
            ArtifactType::Executable => Script::go_compile_executable(),
            _ => Script::go_compile(),
        };
        vec![
            (
                Task::new(
                    TaskName::new("go-format"),
                    Script::go_format(),
                    DeclaredTaskInput::new(FileSetPattern::new(["**/*.go"])),
                    ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                    TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
                    ParameterDeclarations::default(),
                ),
                Step::new("format"),
            ),
            (
                Task::new(
                    TaskName::new("go-compile"),
                    compile_script,
                    DeclaredTaskInput::new(go_source_superset()),
                    ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                    TaskOutput::new(FileSetPattern::new(["**/*"])),
                    ParameterDeclarations::default(),
                ),
                Step::new("compile"),
            ),
            (
                Task::new(
                    TaskName::new("go-test"),
                    Script::go_test(),
                    DeclaredTaskInput::new(go_source_superset()),
                    ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
                    TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
                    ParameterDeclarations::default(),
                ),
                Step::new("test"),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parameter::ParameterValues;
    use crate::runtime::DummyRuntime;
    use crate::script::Command;
    use crate::types::AbsoluteDirectory;
    use crate::types::RelativeDirectory;
    use crate::types::WorkspaceRoot;
    use smol_str::SmolStr;
    use std::path::PathBuf;

    fn task_named<'tasks>(tasks: &'tasks [(Task, Step)], name: &str) -> &'tasks Task {
        tasks
            .iter()
            .find(|(task, _): &&(Task, Step)| -> bool { task.name() == &TaskName::new(name) })
            .map(|(task, _): &(Task, Step)| -> &Task { task })
            .expect("expected a task with this name")
    }

    fn step_named<'tasks>(tasks: &'tasks [(Task, Step)], name: &str) -> &'tasks Step {
        tasks
            .iter()
            .find(|(task, _): &&(Task, Step)| -> bool { task.name() == &TaskName::new(name) })
            .map(|(_, step): &(Task, Step)| -> &Step { step })
            .expect("expected a task with this name")
    }

    #[test]
    fn go_plugin_binds_each_task_to_the_correct_step() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        assert_eq!(step_named(&tasks, "go-format"), &Step::new("format"));
        assert_eq!(step_named(&tasks, "go-compile"), &Step::new("compile"));
        assert_eq!(step_named(&tasks, "go-test"), &Step::new("test"));
    }

    fn resolve(task: &Task) -> Vec<Command> {
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let workspace_root: WorkspaceRoot = WorkspaceRoot::new(AbsoluteDirectory::new(PathBuf::from("/workspace")));
        task.resolve(
            &ParameterValues::default(),
            &RelativeDirectory::new(""),
            &AbsoluteDirectory::new(PathBuf::from("/workspace/.target/out")),
            &workspace_root,
            &runtime,
        )
        .unwrap()
    }

    #[test]
    fn executable_go_compile_writes_a_binary_to_the_output_directory() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        let commands: Vec<Command> = resolve(task_named(&tasks, "go-compile"));
        assert_eq!(
            commands[0].arguments(),
            &[
                SmolStr::new("build"),
                SmolStr::new("-o"),
                SmolStr::new("/workspace/.target/out/"),
                SmolStr::new("./...")
            ]
        );
    }

    #[test]
    fn library_go_compile_omits_the_output_binary() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Library);
        let commands: Vec<Command> = resolve(task_named(&tasks, "go-compile"));
        assert_eq!(commands[0].arguments(), &[SmolStr::new("build"), SmolStr::new("./...")]);
    }

    #[test]
    fn go_format_and_go_test_declare_no_parameters() {
        let tasks: Vec<(Task, Step)> = GoPlugin::tasks(&ArtifactType::Executable);
        assert!(task_named(&tasks, "go-format").declared_parameters().is_empty());
        assert!(task_named(&tasks, "go-test").declared_parameters().is_empty());
    }
}
