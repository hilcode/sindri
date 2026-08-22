use crate::file_set::FileSetPattern;
use crate::module::BinaryName;
use crate::module::ModuleToolReference;
use crate::parameter::ParameterDeclarations;
use crate::script::Script;
use crate::task::DeclaredTaskInput;
use crate::task::ManagedTaskInput;
use crate::task::Task;
use crate::task::TaskName;
use crate::task::TaskOutput;
use crate::types::AbsoluteFile;
use crate::types::ModuleIdentity;
use std::collections::HashMap;

/// The resolved location of every `module_tools` binary built so far in the current `sindri
/// compile`. Built incrementally as tool-target modules finish their `package` step; a module's own
/// `module_tools` references are always already registered here by the time that module's turn comes,
/// because tool-target modules are visited before any module that references them
/// ([`crate::module_graph::ModuleGraph`]'s dependency-first-and-tool-first order).
#[derive(Debug, Default)]
pub struct ModuleToolBinaries(HashMap<(ModuleIdentity, BinaryName), AbsoluteFile>);

impl ModuleToolBinaries {
    pub fn new() -> ModuleToolBinaries {
        ModuleToolBinaries(HashMap::new())
    }

    pub fn insert(&mut self, module: ModuleIdentity, binary: BinaryName, file: AbsoluteFile) {
        self.0.insert((module, binary), file);
    }

    pub fn resolve(&self, reference: &ModuleToolReference) -> Option<&AbsoluteFile> {
        self.0.get(&(reference.module().clone(), reference.binary().clone()))
    }
}

/// The task Sindri synthesizes for a module's `module_tools` entry: it simply runs the referenced
/// binary, resolved through `inputs."module-tools"` ([`crate::script::ScriptInputs`]) rather than a
/// `PATH` search, so Sindri never needs to guess which on-disk binary a bare name means. The caller
/// binds it to the `generate` step — the one step permitted to write into the module's source tree —
/// so a later `compile` step sees whatever it generates.
pub fn invocation_task(reference: &ModuleToolReference) -> Task {
    let binary: &BinaryName = reference.binary();
    let script: Script = Script::new(format!(
        "fun inputs => [ {{ program = inputs.\"module-tools\".\"{binary}\" }} ]"
    ));
    Task::new(
        TaskName::new(format!("module-tool-{binary}")),
        script,
        DeclaredTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
        ManagedTaskInput::new(FileSetPattern::new(Vec::<&str>::new())),
        TaskOutput::new(FileSetPattern::new(Vec::<&str>::new())),
        ParameterDeclarations::default(),
    )
    .with_module_tools([reference.clone()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_tool_binaries_resolves_an_inserted_entry() {
        let mut binaries: ModuleToolBinaries = ModuleToolBinaries::new();
        let reference: ModuleToolReference = ModuleToolReference::parse("//tools/codegen:codegen").unwrap();
        let file: AbsoluteFile =
            AbsoluteFile::new(std::path::PathBuf::from("/workspace/.target/tools/codegen/bin/codegen"));
        binaries.insert(reference.module().clone(), reference.binary().clone(), file.clone());
        assert_eq!(binaries.resolve(&reference), Some(&file));
    }

    #[test]
    fn module_tool_binaries_returns_none_for_an_unregistered_reference() {
        let binaries: ModuleToolBinaries = ModuleToolBinaries::new();
        let reference: ModuleToolReference = ModuleToolReference::parse("//tools/codegen:codegen").unwrap();
        assert_eq!(binaries.resolve(&reference), None);
    }

    #[test]
    fn invocation_task_is_bound_to_generate_and_carries_the_reference() {
        let reference: ModuleToolReference = ModuleToolReference::parse("//tools/codegen:codegen").unwrap();
        let task: Task = invocation_task(&reference);
        assert_eq!(task.name().to_string(), "module-tool-codegen");
        assert_eq!(task.module_tools(), &[reference]);
        assert_eq!(
            task.script().source(),
            "fun inputs => [ { program = inputs.\"module-tools\".\"codegen\" } ]"
        );
    }
}
