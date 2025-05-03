#!/bin/bash

LOGFILE=h3server.log
if ! [ -e "h3spec-linux-x86_64" ] ; then
    # if we don't already have a h3spec executable, wget it from github
    wget https://github.com/kazu-yamamoto/h3spec/releases/download/v0.1.13/h3spec-linux-x86_64
    chmod +x h3spec-linux-x86_64
fi

# Build the server example
cargo build --example server

# Start the server example 
./target/debug/examples/server --listen=[::]:4433 &> $LOGFILE & 
SERVER_PID=$!

sleep 1s

# Run the test 
./h3spec-linux-x86_64 localhost 4433 -n \
    --skip "/QUIC servers/MUST send missing_extension TLS alert if the quic_transport_parameters extension does not included [TLS 8.2]/" \
    --skip "/HTTP/3 servers/MUST send H3_FRAME_UNEXPECTED if CANCEL_PUSH is received in a request stream [HTTP/3 7.2.5]/" \
    --skip "/HTTP/3 servers/MUST send QPACK_ENCODER_STREAM_ERROR if a new dynamic table capacity value exceeds the limit [QPACK 4.1.3]/" \
    --skip "/HTTP/3 servers/MUST send QPACK_DECODER_STREAM_ERROR if Insert Count Increment is 0 [QPACK 4.4.3]/" \
    --skip "/HTTP/3 servers/MUST send H3_MESSAGE_ERROR if a pseudo-header is duplicated [HTTP/3 4.1.1]/" \

H3SPEC_STATUS=$?

if [ "${H3SPEC_STATUS}" -eq 0 ]; then
    echo "h3spec passed!"
else
    echo "h3spec failed! Server Logs:"
    cat $LOGFILE
fi
kill "${SERVER_PID}"
exit "${H3SPEC_STATUS}"
