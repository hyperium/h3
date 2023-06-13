#!/usr/bin/env bash

set -e

duvet report \
    --spec-pattern 'specs/**/*.toml' \
    --spec-pattern 'ci/compliance/specs/**/*.toml' \
    --source-pattern 'sec-http3/**/*.rs' \
    --workspace \
    --exclude duvet \
    --require-tests false \
    --blob-link "https://github.com/security-union/sec-http3/blob/master" \
    --issue-link 'https://github.com/security-union/sec-http3/issues' \
    --no-cargo \
    --html ci/compliance/report.html

echo "compliance report available in 'ci/compliance/report.html'"
