use crate::parameter::PluginName;
use crate::task::Task;
use crate::types::Step;

/// A plugin's tasks, each bound to the lifecycle step it runs at — the plugin analogue of what
/// [`crate::lifecycle::Lifecycle`] is to [`Step`]. Where `Lifecycle` owns the ordered step list a
/// build walks, `Plugin` owns the tasks a language contributes at each of those steps.
#[derive(Debug)]
pub struct Plugin {
    name: PluginName,
    tasks: Vec<(Task, Step)>,
}

impl Plugin {
    pub fn new(name: PluginName, tasks: Vec<(Task, Step)>) -> Plugin {
        Plugin { name, tasks }
    }

    pub fn name(&self) -> &PluginName {
        &self.name
    }

    pub fn tasks(&self) -> &[(Task, Step)] {
        &self.tasks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_set::FileSetPattern;
    use crate::parameter::ParameterDeclarations;
    use crate::script::Script;
    use crate::task::DeclaredTaskInput;
    use crate::task::ManagedTaskInput;
    use crate::task::TaskName;
    use crate::task::TaskOutput;

    fn make_task(name: &str) -> Task {
        Task::new(
            TaskName::new(name),
            Script::new("fun inputs => []"),
            DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
            TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
            ParameterDeclarations::default(),
        )
    }

    #[test]
    fn a_plugin_exposes_its_name_and_tasks_bound_to_steps() {
        let tasks: Vec<(Task, Step)> = vec![
            (make_task("go-format"), Step::new("format")),
            (make_task("go-compile"), Step::new("compile")),
        ];
        let plugin: Plugin = Plugin::new(PluginName::new("sindri-go"), tasks);
        assert_eq!(plugin.name(), &PluginName::new("sindri-go"));
        let names: Vec<String> = plugin
            .tasks()
            .iter()
            .map(|(task, _): &(Task, Step)| -> String { task.name().to_string() })
            .collect();
        assert_eq!(names, vec!["go-format", "go-compile"]);
        let steps: Vec<&Step> = plugin.tasks().iter().map(|(_, step): &(Task, Step)| step).collect();
        assert_eq!(steps, vec![&Step::new("format"), &Step::new("compile")]);
    }

    #[test]
    fn a_plugin_with_no_tasks_exposes_an_empty_slice() {
        let plugin: Plugin = Plugin::new(PluginName::new("sindri-go"), Vec::new());
        assert!(plugin.tasks().is_empty());
    }
}
