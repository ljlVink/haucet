use std::process::Command;

fn extractor() -> Command {
    Command::new(env!("CARGO_BIN_EXE_extract-erofs"))
}

#[test]
fn help_exits_successfully() {
    let output = extractor().arg("--help").output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn version_exits_successfully() {
    let output = extractor().arg("--version").output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains(erofs::VERSION));
}
