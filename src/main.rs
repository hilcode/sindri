use clap::Parser;

const LONG_ABOUT: &str = "\
Sindri is a build tool that combines the best properties of existing
build systems while avoiding their principal failure modes.

Design principles:
  Declarative — build files describe what a module is, not how to build it.
  Explicit    — dependencies, plugin ordering, and tool requirements are all declared.
  Correct     — inputs and outputs are tracked precisely; work is skipped safely.
  Recoverable — the tool can recover from any build failure without a full clean rebuild.
  Extensible  — new languages and tools are added through a typed plugin API.
  IDE-first   — Build Server Protocol (BSP) support is a core feature.\
";

#[derive(Parser)]
#[command(
    version,
    about = "A declarative, lifecycle-driven build tool with incremental correctness.",
    long_about = LONG_ABOUT
)]
struct Arguments {}

fn main() {
    let _arguments: Arguments = Arguments::parse();
}
