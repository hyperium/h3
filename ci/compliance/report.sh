#!/usr/bin/env bash

set -e

BLOB=${1:-master}

./target/release/duvet \
  report \
  --spec-pattern 'specs/**/*.toml' \
  --source-pattern 'h3/**/*.rs' \
  --source-pattern 'tests/**/*.rs' \
  --workspace \
  --exclude duvet \
  --require-tests false \
  --blob-link "https://github.com/hyperium/h3/blob/$BLOB" \
  --issue-link 'https://github.com/hyperium/h3/issues' \
  --no-cargo \
  --html ci/compliance/report.html

echo "compliance report available in 'ci/compliance/report.html'"
