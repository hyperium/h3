use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if std::thread::panicking() {
            println!("Cleaning up child process while unwinding");
            if let Err(e) = self.0.kill() {
                println!("Failed to kill child process: {}", e);
            }
        }
    }
}

#[test]
fn server_and_client_should_connect_successfully() {
    // A little hack since CARGO_BIN_EXE_<name> is not set for examples
    let mut command = PathBuf::from(std::env!("CARGO_MANIFEST_DIR"));
    command.push("../target/debug/examples/server");

    let server = Command::new(command.as_path())
        .arg("--listen=[::]:4433")
        .arg("--cert=../examples/server.cert")
        .arg("--key=../examples/server.key")
        .spawn()
        .expect("Failed to run server example");

    let mut server = ChildGuard(server);

    assert!(server.0.stderr.is_none(), "Failed to listen on localhost");

    command.pop();
    command.push("client");

    assert!(
        Command::new(command)
            .arg("https://localhost:4433")
            .arg("--ca=../examples/ca.cert")
            .stderr(Stdio::null())
            .status()
            .expect("Failed to run client example")
            .success(),
        "Failed to connect to server"
    );

    server.0.kill().expect("Failed to terminate server");
}
