# SCEP CAs made by the pinned scepserver

Each directory is the output of `scepserver ca -init` of scepserver 2.3.0, the linux-amd64 archive that
`crates/hostkeeper-mdm-core/pins.txt` pins (sha256 `cb429621a5dee8acaa19012b42e8126699a13469fe47e8a264793d6c4ba0f845`), run as:

    scepserver-linux-amd64 ca -init -depot <dir> -years 10 -common_name "Example MDM SCEP CA" \
        -organization Example -organizational_unit hostkeeper-mdm

with the arguments the kit passes for the example settings (`scep_ca_init_args`):

- `a` and `b`: as above, two CAs with different keys;
- `password`: as above with `-key-password other`, which the kit never passes.

`ca.key` is a PKCS#1 RSA-4096 key in PEM, encrypted (DES-EDE3-CBC) with the `-key-password`,
the empty one when none is given; `ca.pem` is its self-signed certificate. They are test
fixtures only: no device or service trusts them. `tests/scep_depot_integration.rs` reads
them with the real openssl.
