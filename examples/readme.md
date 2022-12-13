# Getting started 
This is a quick starting guide to run the example.

## Start the Server
To start the example server you can run following command:

```bash
> cargo run --example server -- --listen=127.0.0.1:4433
```

This will start an the HTTP/3 server, listening to 127.0.0.1 on port 4433.  
This also generates a self-signed certificate for encryption.

## Start the client
To start the example client you can run following command:

```bash
> cargo run --example client -- https://localhost:4433
```

This sends an HTTP request to the server.  

## Add some content to the Server
So that the server responds something you can provide a directory with content files.

```bash
> cargo run --example server -- --listen=127.0.0.1:4433 --dir=content/root
```

To start the client simply put the file name behind the URI:

```bash
> cargo run --example client -- https://localhost:4433/index.html
```

## Test against the Browser 
The first step is to run the server.  
For Browsers to work the server have to listen to ipv6 (`--listen=[::]:4433 `).  
Also the browser need a valid certificate (`--cert=examples/cert.der --key=examples/key.der`).  

```bash
> cargo run --example server -- --listen=[::]:4433 --dir=examples/root --cert=examples/cert.der --key=examples/key.der
```

Then run chromium and force it to use Quic.
```bash
> chromium --enable-quic --quic-version=h3 --origin-to-force-quic-on=localhost:4433
```

Now you can navigate to files in the `root` folder for example `https://localhost:4433/index.html`.

## Debugging
The example [example client](client.rs) can generate a `SSLKEYLOGFILE` to see the traffic unencrypted in tools like Wireshark.  
To set this up just set the `SSLKEYLOGFILE` environment variable to a file path and follow this [tutorial](https://wiki.wireshark.org/TLS#using-the-pre-master-secret).
Then use the example client with the `--keylogfile=true` option to enable this.
