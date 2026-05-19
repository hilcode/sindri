use crate::types::Step;
use smol_str::SmolStr;
use std::fmt;

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

impl fmt::Display for TaskName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
pub struct PluginName(SmolStr);

impl PluginName {
    pub fn new(name: impl Into<SmolStr>) -> Self {
        Self(name.into())
    }
}

impl fmt::Display for PluginName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
pub struct ShellCommand(SmolStr);

impl ShellCommand {
    pub fn new(command: impl Into<SmolStr>) -> Self {
        Self(command.into())
    }
}

impl fmt::Display for ShellCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
pub struct Glob(SmolStr);

impl Glob {
    pub fn new(pattern: impl Into<SmolStr>) -> Self {
        Self(pattern.into())
    }
}

impl fmt::Display for Glob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
pub struct Task {
    pub name: TaskName,
    pub step: Step,
    pub command: ShellCommand,
    pub inputs: Vec<Glob>,
    pub outputs: Vec<Glob>,
}

#[derive(Debug)]
pub struct Plugin {
    pub name: PluginName,
    pub tasks: Vec<Task>,
}

pub fn go_plugin() -> Plugin {
    Plugin {
        name: PluginName::new("sindri-go"),
        tasks: vec![
            Task {
                name: TaskName::new("go-format"),
                step: Step::new("format"),
                command: ShellCommand::new("gofmt -l ."),
                inputs: vec![Glob::new("**/*.go")],
                outputs: vec![],
            },
            Task {
                name: TaskName::new("go-compile"),
                step: Step::new("compile"),
                command: ShellCommand::new("go build ./..."),
                inputs: vec![Glob::new("**/*.go"), Glob::new("go.mod"), Glob::new("go.sum")],
                outputs: vec![],
            },
            Task {
                name: TaskName::new("go-test"),
                step: Step::new("test"),
                command: ShellCommand::new("go test ./..."),
                inputs: vec![Glob::new("**/*.go"), Glob::new("go.mod"), Glob::new("go.sum")],
                outputs: vec![],
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_plugin_contributes_to_correct_lifecycle_steps() {
        let plugin: Plugin = go_plugin();
        let tasks_in = |step_name: &str| -> Vec<&Task> {
            let step: Step = Step::new(step_name);
            plugin.tasks.iter().filter(|task| task.step == step).collect()
        };
        let format_tasks: Vec<&Task> = tasks_in("format");
        let compile_tasks: Vec<&Task> = tasks_in("compile");
        let test_tasks: Vec<&Task> = tasks_in("test");
        assert_eq!(format_tasks.len(), 1);
        assert_eq!(format_tasks[0].name, TaskName::new("go-format"));
        assert_eq!(compile_tasks.len(), 1);
        assert_eq!(compile_tasks[0].name, TaskName::new("go-compile"));
        assert_eq!(test_tasks.len(), 1);
        assert_eq!(test_tasks[0].name, TaskName::new("go-test"));
    }
}
