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
        r#"{ name = "my-app", language = "go", type = "executable", version = "0.1.0" }"#,
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
