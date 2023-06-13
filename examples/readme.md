# Getting started 
This is a quick starting guide to run the WebTransport example.

## Generate the required certs/keys and launch chrome
```
cd examples
./launch_chrome.sh
```

whithin this folder run:
```
RUST_LOG=debug cargo run --example webtransport_server -- --listen=127.0.0.1:4433 --key localhost_key.der --cert localhost.der
```

Notice that Chrome will accept the generated keys only for 127.0.0.1:44333 so connecting to localhost:4433 won't work.

Head to https://security-union.github.io/yew-webtransport/

Change the server endpoint to `https://127.0.0.1:4433` try to send datagrams, the sample server should echo them to the client.


