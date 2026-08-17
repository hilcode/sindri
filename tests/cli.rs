use sindri::parameter::Parameter;
use sindri::parameter::ParameterBinding;
use sindri::parameter::ParameterDeclarations;
use sindri::parameter::ParameterName;
use sindri::parameter::ParameterState;
use sindri::parameter::ParameterType;
use sindri::parameter::ParameterValue;
use sindri::parameter::PluginName;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Output;
use tempfile::TempDir;

fn sindri() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sindri"))
}

fn expected_version_output() -> String {
    format!("sindri {}", env!("CARGO_PKG_VERSION"))
}

/// The binding hash a parameter-less task (`generate-go-work`) resolves to.
fn empty_binding_hash() -> String {
    ParameterBinding::empty().binding_hash().to_hex()
}

/// The binding hash `go-compile` resolves to for a given `sindri-go.mode` value. Every Go module
/// below binds `mode = "debug"` unless it's specifically exercising a second, coexisting binding, so
/// `mode_binding_hash("debug")` is the binding-hash path segment shared by most fixtures here.
fn mode_binding_hash(mode: &str) -> String {
    let declared: ParameterDeclarations = ParameterDeclarations::new([Parameter::new(
        PluginName::new("sindri-go"),
        ParameterName::new("mode"),
        ParameterType::new("String"),
    )]);
    let values: ParameterState = ParameterState::new([(
        PluginName::new("sindri-go"),
        ParameterName::new("mode"),
        ParameterValue::new(format!("\"{mode}\"")),
    )]);
    ParameterBinding::resolve(&declared, &values)
        .unwrap()
        .binding_hash()
        .to_hex()
}

fn workspace_dir() -> TempDir {
    let directory: TempDir = TempDir::new().unwrap();
    fs::write(
        directory.path().join("sindri.workspace"),
        r#"{ name = "test", sindri_version = "0.1.0" }"#,
    )
    .unwrap();
    directory
}

fn module_dir() -> TempDir {
    let directory: TempDir = workspace_dir();
    fs::write(
        directory.path().join("sindri.build"),
        r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0",
             parameters = { "sindri-go" = { mode = "debug" } } }"#,
    )
    .unwrap();
    directory
}

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("testdata")
}

fn go_module_dir() -> TempDir {
    let directory: TempDir = module_dir();
    let source: PathBuf = testdata_dir().join("go-module");
    fs::copy(source.join("main.go"), directory.path().join("main.go")).unwrap();
    fs::copy(source.join("go.mod"), directory.path().join("go.mod")).unwrap();
    directory
}

fn failing_go_module_dir() -> TempDir {
    let directory: TempDir = module_dir();
    let source: PathBuf = testdata_dir().join("failing-go-module");
    fs::copy(source.join("main.go"), directory.path().join("main.go")).unwrap();
    fs::copy(source.join("go.mod"), directory.path().join("go.mod")).unwrap();
    directory
}

/// A workspace whose entry `app` executable declares a `{ module = … }` dependency on a local
/// `//lib/greeting` library. `app` imports `example.com/greeting`, which only resolves once Sindri
/// generates the `go.work` covering both module directories.
fn multi_module_dir() -> TempDir {
    let directory: TempDir = workspace_dir();
    fs::write(
        directory.path().join("sindri.build"),
        r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
             dependencies = { compile = [ { module = "//lib/greeting" } ] },
             parameters = { "sindri-go" = { mode = "debug" } } }"#,
    )
    .unwrap();
    fs::write(directory.path().join("go.mod"), "module example.com/app\n\ngo 1.21\n").unwrap();
    fs::write(
        directory.path().join("main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/greeting\"\n)\n\nfunc main() {\n\tfmt.Println(greeting.Message(\"Sindri\"))\n}\n",
    )
    .unwrap();
    let greeting: PathBuf = directory.path().join("lib").join("greeting");
    fs::create_dir_all(&greeting).unwrap();
    fs::write(
        greeting.join("sindri.build"),
        r#"{ name = "greeting", language = "go", type = "library", version = "0.1.0",
             parameters = { "sindri-go" = { mode = "debug" } } }"#,
    )
    .unwrap();
    fs::write(greeting.join("go.mod"), "module example.com/greeting\n\ngo 1.21\n").unwrap();
    fs::write(
        greeting.join("greeting.go"),
        "package greeting\n\nimport \"fmt\"\n\nfunc Message(name string) string {\n\treturn fmt.Sprintf(\"Hello, %s!\", name)\n}\n",
    )
    .unwrap();
    directory
}

/// A workspace whose entry `app` executable declares `module_tools = [ "//tools/codegen:codegen" ]`.
/// The `codegen` binary, built from `tools/codegen/`, writes `generated_greeting.go` — defining
/// `generatedGreeting`, which `app`'s `main.go` calls — into `app`'s own directory when Sindri runs it
/// during `generate`, before `app`'s own `compile` step needs it.
fn codegen_dir() -> TempDir {
    let directory: TempDir = workspace_dir();
    fs::write(
        directory.path().join("sindri.build"),
        r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
             module_tools = [ "//tools/codegen:codegen" ],
             parameters = { "sindri-go" = { mode = "debug" } } }"#,
    )
    .unwrap();
    fs::write(directory.path().join("go.mod"), "module example.com/app\n\ngo 1.21\n").unwrap();
    fs::write(
        directory.path().join("main.go"),
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n\tfmt.Println(generatedGreeting())\n}\n",
    )
    .unwrap();
    let codegen: PathBuf = directory.path().join("tools").join("codegen");
    fs::create_dir_all(&codegen).unwrap();
    fs::write(
        codegen.join("sindri.build"),
        r#"{ name = "codegen", language = "go", type = "executable", version = "0.1.0",
             parameters = { "sindri-go" = { mode = "debug" } } }"#,
    )
    .unwrap();
    fs::write(codegen.join("go.mod"), "module example.com/codegen\n\ngo 1.21\n").unwrap();
    fs::write(
        codegen.join("main.go"),
        "package main\n\nimport \"os\"\n\nconst generated = \"package main\\n\\nfunc generatedGreeting() string { return \\\"Hello from codegen!\\\" }\\n\"\n\nfunc main() {\n\tif err := os.WriteFile(\"generated_greeting.go\", []byte(generated), 0o644); err != nil {\n\t\tpanic(err)\n\t}\n}\n",
    )
    .unwrap();
    directory
}

/// Like [`multi_module_dir`], but with the executable in its own `app/` subdirectory, disjoint from the
/// library's `lib/` directory, so neither module's source glob sweeps the other. This isolates the
/// dependency edge: only edge propagation — not input-glob overlap — can tie `app`'s freshness to the
/// library's, which is exactly what multi-module incrementality must guarantee.
fn isolated_multi_module_dir() -> TempDir {
    let directory: TempDir = workspace_dir();
    let app: PathBuf = directory.path().join("app");
    fs::create_dir_all(&app).unwrap();
    fs::write(
        app.join("sindri.build"),
        r#"{ name = "app", language = "go", type = "executable", version = "0.1.0",
             dependencies = { compile = [ { module = "//lib" } ] },
             parameters = { "sindri-go" = { mode = "debug" } } }"#,
    )
    .unwrap();
    fs::write(app.join("go.mod"), "module example.com/app\n\ngo 1.21\n").unwrap();
    fs::write(
        app.join("main.go"),
        "package main\n\nimport (\n\t\"fmt\"\n\n\t\"example.com/greeting\"\n)\n\nfunc main() {\n\tfmt.Println(greeting.Message(\"Sindri\"))\n}\n",
    )
    .unwrap();
    let lib: PathBuf = directory.path().join("lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("sindri.build"),
        r#"{ name = "greeting", language = "go", type = "library", version = "0.1.0",
             parameters = { "sindri-go" = { mode = "debug" } } }"#,
    )
    .unwrap();
    fs::write(lib.join("go.mod"), "module example.com/greeting\n\ngo 1.21\n").unwrap();
    fs::write(lib.join("greeting.go"), greeting_source("Hello")).unwrap();
    directory
}

/// The library source, parameterised by the word its `Message` greets with, so a test can edit the
/// dependency's behaviour and observe whether the dependent picked up the change.
fn greeting_source(word: &str) -> String {
    format!(
        "package greeting\n\nimport \"fmt\"\n\nfunc Message(name string) string {{\n\treturn fmt.Sprintf(\"{word}, %s!\", name)\n}}\n"
    )
}

/// The lone binary the `app` executable's `go-compile` writes into its own tracked output directory
/// (`.target/app/compile/go-compile/output/`), read back so a test can tell whether it was rebuilt.
fn app_binary(workspace: &Path) -> Vec<u8> {
    let output_directory: PathBuf = workspace
        .join(".target")
        .join("app")
        .join("go-compile")
        .join(mode_binding_hash("debug"));
    let entry: fs::DirEntry = fs::read_dir(&output_directory)
        .unwrap_or_else(|error| panic!("output directory {output_directory:?} is unreadable: {error}"))
        .next()
        .expect("the executable's compile should leave exactly one binary")
        .unwrap();
    fs::read(entry.path()).unwrap()
}

#[test]
fn version_long_flag() {
    let output: Output = sindri().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected_version_output()
    );
}

#[test]
fn version_short_flag() {
    let output: Output = sindri().arg("-V").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected_version_output()
    );
}

#[test]
fn help_outside_a_workspace_lists_the_embedded_default_and_clean_steps() {
    let directory: TempDir = TempDir::new().unwrap();
    let output: Output = sindri().arg("--help").current_dir(directory.path()).output().unwrap();
    assert!(output.status.success());
    let stdout: String = String::from_utf8_lossy(&output.stdout).into_owned();
    for step in [
        "generate",
        "format",
        "compile",
        "package",
        "publish",
        "clean",
        "lifecycle",
    ] {
        assert!(
            stdout.contains(step),
            "expected `--help` to list `{step}`, got:\n{stdout}"
        );
    }
}

#[test]
fn help_inside_a_workspace_lists_the_same_steps_sourced_from_the_bootstrapped_lifecycles() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri().arg("--help").current_dir(directory.path()).output().unwrap();
    assert!(output.status.success());
    let stdout: String = String::from_utf8_lossy(&output.stdout).into_owned();
    for step in [
        "generate",
        "format",
        "compile",
        "package",
        "publish",
        "clean",
        "lifecycle",
    ] {
        assert!(
            stdout.contains(step),
            "expected `--help` to list `{step}`, got:\n{stdout}"
        );
    }
    assert!(
        directory.path().join(".sindri/lifecycles/checksums.json").is_file(),
        "expected `--help` inside a workspace to bootstrap .sindri/lifecycles/"
    );
}

#[test]
fn log_flag_creates_log_file() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri()
        .args(["--log", "lifecycle"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let log_file: PathBuf = directory.path().join(".target").join("sindri.log");
    assert!(log_file.exists(), "log file was not created");
    let log_contents: String = fs::read_to_string(&log_file).unwrap();
    assert!(!log_contents.is_empty(), "log file is empty");
}

#[test]
fn no_log_flag_creates_no_log_file() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri()
        .arg("lifecycle")
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        !directory.path().join(".target").join("sindri.log").exists(),
        "log file was created without --log"
    );
}

#[test]
fn log_flag_produces_no_terminal_output() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri()
        .args(["--log", "lifecycle"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn lifecycle_shows_only_steps_with_tasks_by_default() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri()
        .arg("lifecycle")
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout: String = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("go-compile"), "missing go-compile task");
    assert!(stdout.contains("go-test"), "missing go-test task");
    assert!(
        !stdout.contains("(no tasks)"),
        "empty steps should be hidden by default"
    );
}

#[test]
fn lifecycle_all_flag_shows_empty_steps() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri()
        .args(["lifecycle", "--all"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout: String = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("go-compile"), "missing go-compile task");
    assert!(stdout.contains("(no tasks)"), "empty steps should appear with --all");
}

#[test]
fn lifecycle_short_all_flag() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri()
        .args(["lifecycle", "-a"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("(no tasks)"),
        "short -a flag should behave identically to --all"
    );
}

#[test]
fn compile_in_valid_go_module_exits_zero() {
    let directory: TempDir = go_module_dir();
    let output: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn format_in_valid_go_module_exits_zero() {
    let directory: TempDir = go_module_dir();
    let output: Output = sindri().arg("format").current_dir(directory.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn clean_removes_everything_except_its_own_state() {
    let directory: TempDir = go_module_dir();
    let compile: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(compile.status.success(), "expected compile to succeed first");
    assert!(
        directory.path().join(".target/go-compile").is_dir(),
        "expected .target/go-compile to exist after compile"
    );

    let clean: Output = sindri().arg("clean").current_dir(directory.path()).output().unwrap();
    assert!(
        clean.status.success(),
        "expected exit 0; stderr: {}",
        String::from_utf8_lossy(&clean.stderr)
    );
    assert!(
        !directory.path().join(".target/go-compile").exists(),
        "expected .target/go-compile to be removed by clean"
    );
    assert!(
        directory.path().join(".target/clean").is_dir(),
        "expected clean's own state under .target/clean to survive its own run"
    );

    // Cleaning an already-clean workspace is not an error.
    let second_clean: Output = sindri().arg("clean").current_dir(directory.path()).output().unwrap();
    assert!(second_clean.status.success(), "expected a second clean to also exit 0");
}

#[test]
fn compile_module_with_local_go_library_dependency_builds() {
    let directory: TempDir = multi_module_dir();
    let output: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 building a module with a local library dependency; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let go_work: String = fs::read_to_string(
        directory
            .path()
            .join(".target")
            .join("generate-go-work")
            .join(empty_binding_hash())
            .join("go.work"),
    )
    .expect("a go.work should be generated in the build directory");
    assert!(
        go_work.contains("lib/greeting"),
        "the generated go.work should list the local library, got:\n{go_work}"
    );
}

#[test]
fn compile_module_with_module_tools_generates_and_compiles_the_tool_output() {
    let directory: TempDir = codegen_dir();
    let output: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 building a module with module_tools; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated: String = fs::read_to_string(directory.path().join("generated_greeting.go"))
        .expect("the codegen binary should have written generated_greeting.go into app's directory");
    assert!(
        generated.contains("Hello from codegen!"),
        "unexpected generated file content:\n{generated}"
    );
    let output_directory: PathBuf = go_compile_output_directory(directory.path());
    let binary_path: PathBuf = fs::read_dir(&output_directory)
        .unwrap_or_else(|error| panic!("output directory {output_directory:?} is unreadable: {error}"))
        .next()
        .expect("app's compile should leave exactly one binary")
        .unwrap()
        .path();
    let run: Output = Command::new(&binary_path).output().unwrap();
    assert!(
        run.status.success(),
        "the compiled binary should run successfully; stderr: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "Hello from codegen!",
        "the compiled binary should print the codegen-generated greeting"
    );
}

#[test]
fn second_multi_module_compile_is_silent_and_editing_the_dependency_rebuilds_the_dependent() {
    let directory: TempDir = isolated_multi_module_dir();
    let app: PathBuf = directory.path().join("app");
    let first: Output = sindri().arg("compile").current_dir(&app).output().unwrap();
    assert!(
        first.status.success(),
        "first compile failed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let original_binary: Vec<u8> = app_binary(directory.path());

    // Nothing changed: the second compile must be silent (both modules cache-hit).
    let second: Output = sindri().arg("compile").current_dir(&app).output().unwrap();
    assert!(second.status.success(), "second compile failed");
    assert!(
        second.stdout.is_empty(),
        "a second compile on an unchanged multi-module tree should be silent; got: {}",
        String::from_utf8_lossy(&second.stdout)
    );

    // Edit only the dependency. The dependent's own sources are untouched and live in a disjoint
    // directory, so only the dependency edge can rebuild it — and it must, or its binary would keep
    // embedding the library's old behaviour.
    fs::write(directory.path().join("lib").join("greeting.go"), greeting_source("Hi")).unwrap();
    let rebuild: Output = sindri().arg("compile").current_dir(&app).output().unwrap();
    assert!(
        rebuild.status.success(),
        "rebuild after editing the dependency failed; stderr: {}",
        String::from_utf8_lossy(&rebuild.stderr)
    );
    assert!(
        String::from_utf8_lossy(&rebuild.stdout).contains("go-compile"),
        "editing the dependency should re-run a compile; got: {}",
        String::from_utf8_lossy(&rebuild.stdout)
    );
    assert_ne!(
        app_binary(directory.path()),
        original_binary,
        "the dependent's binary should change, proving it rebuilt against the edited dependency"
    );
}

#[test]
fn compile_shows_progress_output() {
    let directory: TempDir = go_module_dir();
    let output: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    let stdout: String = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(stdout.contains("go-compile"), "missing go-compile progress line");
    assert!(stdout.contains('\u{2713}'), "missing success checkmark");
}

#[test]
fn compile_quiet_flag_suppresses_progress() {
    let directory: TempDir = go_module_dir();
    let output: Output = sindri()
        .args(["compile", "--quiet"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "expected no stdout with --quiet; got: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn compile_creates_telemetry_json() {
    let directory: TempDir = go_module_dir();
    sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    let telemetry_path: PathBuf = directory.path().join(".target").join("telemetry.json");
    assert!(telemetry_path.exists(), "telemetry.json was not created");
    let content: String = fs::read_to_string(&telemetry_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    let events: &Vec<serde_json::Value> = parsed["traceEvents"].as_array().unwrap();
    assert!(!events.is_empty(), "traceEvents should not be empty");
    for event in events {
        assert!(event["dur"].as_u64().unwrap() > 0, "all dur values should be positive");
    }
}

#[test]
fn compile_failing_go_code_exits_nonzero() {
    let directory: TempDir = failing_go_module_dir();
    let output: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(!output.status.success(), "expected non-zero exit for broken Go code");
    assert!(!output.stderr.is_empty(), "expected error output on stderr");
}

#[test]
fn compile_quiet_with_failure_still_shows_error() {
    let directory: TempDir = failing_go_module_dir();
    let output: Output = sindri()
        .args(["compile", "--quiet"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "expected non-zero exit");
    assert!(
        !output.stderr.is_empty(),
        "expected error output on stderr even with --quiet"
    );
    assert!(output.stdout.is_empty(), "expected no stdout with --quiet");
}

fn go_compile_output_directory(workspace: &Path) -> PathBuf {
    // A plain `sindri.build` module has no qualifier, so its state lives directly under `.target`
    // (no module-path segment): `.target/<task-name>/<binding-hash>/`.
    workspace
        .join(".target")
        .join("go-compile")
        .join(mode_binding_hash("debug"))
}

#[test]
fn second_compile_on_unchanged_tree_is_silent() {
    let directory: TempDir = go_module_dir();
    let first: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(
        first.status.success(),
        "first compile failed; stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        String::from_utf8_lossy(&first.stdout).contains("go-compile"),
        "first compile should run go-compile"
    );
    let output_directory: PathBuf = go_compile_output_directory(directory.path());
    assert!(
        fs::read_dir(&output_directory).unwrap().next().is_some(),
        "first compile should leave a built binary in {output_directory:?}"
    );

    let second: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(second.status.success(), "second compile failed");
    assert!(
        second.stdout.is_empty(),
        "second compile on an unchanged tree should be silent; got: {}",
        String::from_utf8_lossy(&second.stdout)
    );
}

#[test]
fn editing_a_source_file_triggers_a_rebuild() {
    let directory: TempDir = go_module_dir();
    sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    fs::write(
        directory.path().join("main.go"),
        "package main\n\nfunc main() {\n\tprintln(\"changed\")\n}\n",
    )
    .unwrap();
    let rebuild: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(rebuild.status.success(), "rebuild after edit failed");
    assert!(
        String::from_utf8_lossy(&rebuild.stdout).contains("go-compile"),
        "editing a source file should re-run go-compile"
    );
}

#[test]
fn deleting_the_output_binary_triggers_a_rebuild() {
    let directory: TempDir = go_module_dir();
    sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    let output_directory: PathBuf = go_compile_output_directory(directory.path());
    for entry in fs::read_dir(&output_directory).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    let rebuild: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(rebuild.status.success(), "rebuild after deleting output failed");
    assert!(
        String::from_utf8_lossy(&rebuild.stdout).contains("go-compile"),
        "deleting the output binary should re-run go-compile (self-healing)"
    );
}

#[test]
fn two_parameter_bindings_of_one_module_coexist_without_overwriting_each_other() {
    let directory: TempDir = go_module_dir();
    let debug_build: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(
        debug_build.status.success(),
        "debug-mode compile failed; stderr: {}",
        String::from_utf8_lossy(&debug_build.stderr)
    );
    let debug_output_directory: PathBuf = go_compile_output_directory(directory.path());
    assert!(
        fs::read_dir(&debug_output_directory).unwrap().next().is_some(),
        "the debug binding should leave a binary in {debug_output_directory:?}"
    );

    // Switch the same module to release mode and rebuild. This must add a new binding-hash
    // directory alongside the debug one, not overwrite it.
    let build_file: PathBuf = directory.path().join("sindri.build");
    let source: String = fs::read_to_string(&build_file).unwrap();
    fs::write(&build_file, source.replace(r#"mode = "debug""#, r#"mode = "release""#)).unwrap();
    let release_build: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(
        release_build.status.success(),
        "release-mode compile failed; stderr: {}",
        String::from_utf8_lossy(&release_build.stderr)
    );
    let release_output_directory: PathBuf = directory
        .path()
        .join(".target")
        .join("go-compile")
        .join(mode_binding_hash("release"));
    assert!(
        fs::read_dir(&release_output_directory).unwrap().next().is_some(),
        "the release binding should leave a binary in {release_output_directory:?}"
    );
    assert!(
        fs::read_dir(&debug_output_directory).unwrap().next().is_some(),
        "the debug binding's output should still exist after building the release binding"
    );
}

#[test]
fn no_workspace_exits_nonzero() {
    let directory: TempDir = TempDir::new().unwrap();
    let output: Output = sindri()
        .arg("lifecycle")
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "expected non-zero exit without a workspace");
    assert!(!output.stderr.is_empty(), "expected an error message on stderr");
}

#[test]
fn no_build_file_exits_nonzero() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri().arg("compile").current_dir(directory.path()).output().unwrap();
    assert!(!output.status.success(), "expected non-zero exit without a build file");
    assert!(!output.stderr.is_empty(), "expected an error message on stderr");
}

#[test]
fn broken_workspace_exits_nonzero() {
    let directory: TempDir = TempDir::new().unwrap();
    fs::write(
        directory.path().join("sindri.workspace"),
        r#"{ name = "test", sindri_version = }"#,
    )
    .unwrap();
    let output: Output = sindri()
        .arg("lifecycle")
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for malformed workspace"
    );
    assert!(!output.stderr.is_empty(), "expected an error message on stderr");
}

#[test]
fn tampered_lifecycle_file_exits_nonzero() {
    let directory: TempDir = workspace_dir();
    // Bootstrap `.sindri/lifecycles/` first, then tamper with its content — a workspace is present,
    // so this must fail during `Lifecycles::load`, before the command is even built.
    sindri()
        .arg("lifecycle")
        .current_dir(directory.path())
        .output()
        .unwrap();
    let lifecycle_file: PathBuf = directory.path().join(".sindri/lifecycles/default.json");
    assert!(lifecycle_file.is_file(), "expected lifecycle bootstrap to have run");
    fs::write(&lifecycle_file, r#"["tampered"]"#).unwrap();
    let output: Output = sindri()
        .arg("lifecycle")
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for a tampered lifecycle file"
    );
    let stderr: String = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("doesn't match what Sindri shipped"),
        "expected the LifecycleModified message on stderr, got:\n{stderr}"
    );
}
