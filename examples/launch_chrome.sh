#!/bin/bash

set -e

# generate certificate
openssl req -x509 -newkey rsa:2048 -keyout localhost.key -out localhost.pem -days 365 -nodes -subj "/CN=127.0.0.1"

openssl x509 -in localhost.pem -outform der -out localhost.der

openssl rsa -in localhost.key -outform DER -out localhost_key.der

SPKI=`openssl x509 -inform der -in localhost.der -pubkey -noout | openssl pkey -pubin -outform der | openssl dgst -sha256 -binary | openssl enc -base64`

echo "Got cert key $SPKI"

echo "Opening google chrome"

case `uname` in
    (*Linux*)  google-chrome --origin-to-force-quic-on=127.0.0.1:4433 --ignore-certificate-errors-spki-list=$SPKI --enable-logging --v=1 ;;
    (*Darwin*)  open -a "Google Chrome" --args --origin-to-force-quic-on=127.0.0.1:4433 --ignore-certificate-errors-spki-list=$SPKI --enable-logging --v=1 ;;
esac

## Logs are stored to ~/Library/Application Support/Google/Chrome/chrome_debug.log
