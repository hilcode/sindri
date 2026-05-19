use crate::types::ShellCommand;
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
pub struct Glob(SmolStr);

impl Glob {
    pub fn new(pattern: impl Into<SmolStr>) -> Self {
        Self(pattern.into())
    }
}

impl Display for Glob {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Task {
    name: TaskName,
    step: Step,
    command: ShellCommand,
    inputs: Vec<Glob>,
    outputs: Vec<Glob>,
}

impl Task {
    pub fn new(name: TaskName, step: Step, command: ShellCommand, inputs: Vec<Glob>, outputs: Vec<Glob>) -> Task {
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

    pub fn command(&self) -> &ShellCommand {
        &self.command
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
}
