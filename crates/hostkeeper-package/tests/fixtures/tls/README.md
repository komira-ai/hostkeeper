# TLS test fixtures

The certificates and key the hermetic TLS server of `src/net_tls_tests.rs` uses on 127.0.0.1.
Every file here is a test fixture with no other use. `server.key` is a test key: it protects
nothing, and no host trusts `ca.pem` or `other-ca.pem` outside these tests.

- `ca.pem`: the test root CA, valid for 100 years. The tests give it to `UreqTransport::new`.
- `server.pem`, `server.key`: the leaf certificate for the IP address 127.0.0.1 (a `subjectAltName`
  of `IP:127.0.0.1`, `serverAuth`, not a CA) signed by `ca.pem`, and its key.
- `other-ca.pem`: a second root CA that signed nothing, for the unrelated-root case.

They were made once with these commands (OpenSSL 3, working in an empty scratch directory):

```
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ca.key
openssl req -x509 -new -key ca.key -sha256 -days 36500 -subj "/CN=hostkeeper test CA" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out ca.pem
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out server.key
openssl req -new -key server.key -subj "/CN=127.0.0.1" -out server.csr
printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n' > ext.cnf
openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -sha256 -days 36500 \
  -extfile ext.cnf -out server.pem
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out other.key
openssl req -x509 -new -key other.key -sha256 -days 36500 -subj "/CN=hostkeeper unrelated test CA" \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" \
  -out other-ca.pem
```

The CA keys were discarded after signing; only the four files above are kept.
