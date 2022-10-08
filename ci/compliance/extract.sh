#!/usr/bin/env bash

set -e

specs=(
    'https://www.rfc-editor.org/rfc/rfc9114'
)

for spec in "${specs[@]}"
do
    duvet extract \
        $spec \
        --format 'IETF' \
        --out '.' \
        --extension 'toml'
done

echo "compliance checks available in 'specs/'"
