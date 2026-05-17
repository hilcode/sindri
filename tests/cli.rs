use std::fs;
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
    let output: Output = sindri().arg("--log").current_dir(directory.path()).output().unwrap();
    assert!(output.status.success());
    let log_file: PathBuf = directory.path().join(".target").join("sindri.log");
    assert!(log_file.exists(), "log file was not created");
    let log_contents: String = fs::read_to_string(&log_file).unwrap();
    assert!(!log_contents.is_empty(), "log file is empty");
}

#[test]
fn no_log_flag_creates_no_log_file() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri().current_dir(directory.path()).output().unwrap();
    assert!(output.status.success());
    assert!(
        !directory.path().join(".target").join("sindri.log").exists(),
        "log file was created without --log"
    );
}

#[test]
fn log_flag_produces_no_terminal_output() {
    let directory: TempDir = workspace_dir();
    let output: Output = sindri().arg("--log").current_dir(directory.path()).output().unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "unexpected stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "unexpected stderr: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}
