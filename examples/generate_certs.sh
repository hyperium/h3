
# use mkcert to generate certs for localhost
mkcert localhost

# convert certs from pem to der
openssl x509 -outform der -in localhost.pem -out localhost.crt
openssl rsa -outform der -in localhost-key.pem -out localhost.key
