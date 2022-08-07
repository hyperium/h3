#!/bin/bash

LOGFILE=h3server.log
if ! [ -e "h3spec-linux-x86_64" ] ; then
    # if we don't already have a h3spec executable, wget it from github
    wget https://github.com/kazu-yamamoto/h3spec/releases/download/v0.1.8/h3spec-linux-x86_64
fi

# Build the server example
cargo build --example server

# Start the server example 
./target/debug/examples/server --listen=[::]:4433 &> $LOGFILE & 
SERVER_PID=$!

sleep 1s

# Run the test 
./h3spec-linux-x86_64 localhost 4433 -s 0-RTT
H3SPEC_STATUS=$?

if [ "${H3SPEC_STATUS}" -eq 0 ]; then
    echo "h3spec passed!"
else
    echo "h3spec failed! Server Logs:"
    cat $LOGFILE
fi
kill "${SERVER_PID}"
exit "${H3SPEC_STATUS}"
