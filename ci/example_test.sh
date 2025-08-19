#!/bin/bash
set -eo pipefail

# Change to the repository root directory
cd "$(dirname "$0")/.."

# Start the server in the background
echo "Starting server..."
cargo run --example server -- --listen=[::]:4433 --cert=examples/server.cert --key=examples/server.key &
SERVER_PID=$!

# Wait for the server to start
sleep 2

# Function to clean up server process on exit
cleanup() {
  echo "Cleaning up server process..."
  kill $SERVER_PID 2>/dev/null || true
}

# Set up cleanup on script exit
trap cleanup EXIT

# Run the client
echo "Running client..."
cargo run --example client -- https://localhost:4433 --ca=examples/ca.cert

# If we got here, the test succeeded
echo "Server and client connected successfully!"
exit 0