use std::process::Command;

fn sindri() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sindri"))
}

fn expected_version_output() -> String {
    format!("sindri {}", env!("CARGO_PKG_VERSION"))
}

#[test]
fn version_long_flag() {
    let output = sindri().arg("--version").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected_version_output()
    );
}

#[test]
fn version_short_flag() {
    let output = sindri().arg("-V").output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        expected_version_output()
    );
}
