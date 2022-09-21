use std::{
    path::PathBuf,
    process::{Command, Stdio},
};

#[test]
fn server_and_client_should_connect_successfully() {
    // A little hack since CARGO_BIN_EXE_<name> is not set for examples
    let mut command = PathBuf::from(std::env!("CARGO_MANIFEST_DIR"));
    command.push("../target/debug/examples/server");

    let mut server = Command::new(command.as_path())
        .arg("--listen=[::]:4433")
        .spawn()
        .expect("Failed to run server example");
    assert!(server.stderr.is_none(), "Failed to listen on localhost");

    command.pop();
    command.push("client");
    assert!(
        Command::new(command)
            .args(["https://localhost:4433", "--insecure"])
            .stderr(Stdio::null())
            .status()
            .expect("Failed to run client example")
            .success(),
        "Failed to connect to server"
    );

    server.kill().expect("Failed to terminate server");
}
