#!/bin/bash

LOGFILE=h3server.log
if ! [ -e "h3spec-linux-x86_64" ] ; then
    # if we don't already have a h3spec executable, wget it from github
    wget https://github.com/kazu-yamamoto/h3spec/releases/download/v0.1.8/h3spec-linux-x86_64
    chmod +x h3spec-linux-x86_64
fi

# Build the server example
cargo build --example server

# Start the server example 
./target/debug/examples/server --listen=[::]:4433 &> $LOGFILE & 
SERVER_PID=$!

sleep 1s

# Run the test 
./h3spec-linux-x86_64 localhost 4433 \
        --skip "/QUIC servers/MUST send PROTOCOL_VIOLATION if CRYPTO in 0-RTT is received [TLS 8.3]/" \
        --skip "/QUIC servers/MUST send TRANSPORT_PARAMETER_ERROR if original_destination_connection_id is received [Transport 18.2]/" \
        --skip "/QUIC servers/MUST send TRANSPORT_PARAMETER_ERROR if retry_source_connection_id is received [Transport 18.2]/" \
        --skip "/QUIC servers/MUST send PROTOCOL_VIOLATION on no frames [Transport 12.4]/" \
        --skip "/HTTP/3 servers/MUST send H3_MESSAGE_ERROR if a pseudo-header is duplicated [HTTP/3 4.1.1]/" \
        --skip "/HTTP/3 servers/MUST send H3_FRAME_UNEXPECTED if CANCEL_PUSH is received in a request stream [HTTP/3 7.2.5]/" \
        --skip "/HTTP/3 servers/MUST send QPACK_ENCODER_STREAM_ERROR if a new dynamic table capacity value exceeds the limit [QPACK 4.1.3]/" \
        --skip "/HTTP/3 servers/MUST send QPACK_DECODER_STREAM_ERROR if Insert Count Increment is 0 [QPACK 4.4.3]/" \

H3SPEC_STATUS=$?

if [ "${H3SPEC_STATUS}" -eq 0 ]; then
    echo "h3spec passed!"
else
    echo "h3spec failed! Server Logs:"
    cat $LOGFILE
fi
kill "${SERVER_PID}"
exit "${H3SPEC_STATUS}"
