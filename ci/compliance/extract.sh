#!/usr/bin/env bash

set -e

./target/release/duvet \
    extract \
    ci/compliance/specs/rfc9114.txt \
    --format "IETF" \
    --out "." \
    --extension "toml"

echo "compliance checks available in 'ci/compliance/specs/rfc9114/'"
