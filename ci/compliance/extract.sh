#!/usr/bin/env bash

set -e

duvet \
    extract \
    https://www.rfc-editor.org/rfc/rfc9114 \
    --format "IETF" \
    --out "." \
    --extension "toml"

echo "compliance checks available in 'specs/www.rfc-editor.org/rfc/rfc9114/'"
