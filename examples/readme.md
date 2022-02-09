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
> cargo run --example client -- https://localhost:4433 --insecure=true
```

This sends an HTTP request to the server.  
The `--insecure=true` allows the client to accept invalid certificates like the self-signed, which the server has created.

## Add some content to the Server
So that the server responds something you can provide a directory with content files.

```bash
> cargo run --example server -- --listen=127.0.0.1:4433 --dir=content/root
```

To start the client simply put the file name behind the URI:

```bash
> cargo run --example client -- https://localhost:4433/index.html --insecure=true
```