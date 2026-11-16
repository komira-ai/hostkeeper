//! The CA's keys, an operator's public key and the certificate between them: the OpenSSH formats
//! read and written to the byte, and the signature checked with a verifier that is not this
//! module's. The keys of `ED25519`, `ECDSA` and `RSA` are made by `ssh-keygen`, and their
//! fingerprints are what `ssh-keygen -l` printed for them.

use std::cell::Cell;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ring::signature::{ED25519, KeyPair as _, UnparsedPublicKey};

use super::{
    BEGIN, CaKey, END, KeyError, MAGIC, MAX_CA_KEYS, Reader, Spec, UserKey, fingerprint,
    put_string, put_u32,
};

pub(crate) const ED25519_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINSW0c5QIg73t2MjzpzH78ySOEX8uwvfmJPtDPnxtDK9 fx";
const ED25519_PRINT: &str = "SHA256:wWOa+A9yA/9AqP2DOM1BhmqWuCcmVoGXJQBSRvYZro4";
const ECDSA_KEY: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBL5xFNrCOU3xadLpn+rcsiSFdIwLq9gwD6Xcnbh00YJ8JKEqKjmhzlyOjsgzpQKGWTD3JkAclFZp+wkZG0N9KWk= fx";
const ECDSA_PRINT: &str = "SHA256:LeHLf0qk1jbaVdPf+THHQp8D44fOeY4lBWqZUHwZfEk";
const RSA_KEY: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQDJBg/+6a3MKXIqkFg8+iDopR+njDseGQprWcLX/P/+Y27vEY0q4h6soR3KK9DCbHHMVI26gmwh63l4zNbieRfcgFZBQc2bmx0I1+arTkITpKPfa/SKybRKZ7SA8//0P9WRi6bEFwLyZnrJH1YLrjNBnlNA66XoFBKxftOjw19UajW+xL7nnalytZLKDK4R1LLN/bdlxSl8KYYYaQY0OftOl58epLi2znXcbV5KnBVEfS7ywonjRUAjpoUrKIn0e6uuoiA214FUZcBLRmztIxX3XkIqSpvjQqMKd0R/+t+yhAmuBPVfCGFjzH3uVS1QKHIajhxwjhJ/0Brsr6P0fDORsE4T918Jx7k2kTJJDFsIVKKKNG+ARMlem0Nj05tH1dp5I07dZv6JlaWQ23ilebof5XOhhf7dXq7sEV39KV1KwsySHXxVs9tpsJPMleW/+ChhScjXKjo+WHks9ajHKzH+ZC1Iwd1aOUTq6zqR6yNrxguRgtpV6cd2aiV5oTx0CVU= fx";
const RSA_PRINT: &str = "SHA256:J+dp+kSHxW/1GgHoO7l6tRMx2HNlAFjKcJ6rnJT8AtE";
/// A key of a curve the CA does not sign, and an RSA key of 2048 bits.
const ECDSA_384_KEY: &str = "ecdsa-sha2-nistp384 AAAAE2VjZHNhLXNoYTItbmlzdHAzODQAAAAIbmlzdHAzODQAAABhBEr9yeKODi3cdIuwp2Cr2ygnlsdrOrAgmjjMHBo2TBAMvWB+twazzA+ch/47rzA9bTbz551s1axfHPWgsSFCWU2Dx3Bkr8WE5ikMtj6A/BBdta7A1khRSnP/vHC9DpqmVg== fx";
const RSA_2048_KEY: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQC7DjHBm6kXQ92C5LtDk0w2ic/A08HWb1Se81u5x4NLhzVb/nFEMEKqu4Q1E/bGxgvvWmlhcmJRvXHM1ujgEesCegMvjPRbA4Y1BQWWbpRsIU9LIPxojqqVK7P3qRhgPX2/IZIKnWF9yNE/3NQ3JjzJKLj6/My26CxZDTS0dPLTJBURv7RZoXRA+ic6BJ7HDJTdUUFEN14u7UeoIbG2LXO6ba9RPiObaA7T4JysQS2gUu0HAzULZjh6YbfswWcROeohnkg8FtzWAd0SWs6q7XkCnpUHeR4Ytcj33G5opfGj7rcjnN4JiAGKgfHugvFepo3iNDCs30BJgI0iI59wSj/x fx";

/// Bytes that count up from `start`, so a test's keys and nonces are the same every run.
pub(crate) fn counting(start: u8) -> impl Fn(&mut [u8]) -> bool {
    let next = Cell::new(start);
    move |buffer| {
        for byte in buffer {
            *byte = next.get();
            next.set(next.get().wrapping_add(1));
        }
        true
    }
}

fn nothing(_: &mut [u8]) -> bool {
    false
}

fn generated(start: u8) -> (CaKey, String) {
    CaKey::generate("test-ca", &counting(start)).expect("a key")
}

/// The blob of the public key line `line`.
fn blob_of(line: &str) -> Vec<u8> {
    STANDARD
        .decode(line.split_whitespace().nth(1).expect("a blob"))
        .expect("base64")
}

fn line_of(kind: &str, blob: &[u8]) -> String {
    format!("{kind} {}", STANDARD.encode(blob))
}

/// The private key file with every part a test may break: the cipher and kdf names, the key
/// count, the second check number, the key type, the public key, the 64 secret bytes, and the
/// bytes after the comment.
struct File<'a> {
    cipher: &'a [u8],
    kdf: &'a [u8],
    keys: u32,
    check_two: u32,
    kind: &'a [u8],
    public: &'a [u8],
    secret: Vec<u8>,
    padding: Vec<u8>,
    extra: &'a [u8],
}

impl File<'_> {
    /// The well formed file of the key made from `seed`.
    fn of(seed: u8) -> (File<'static>, CaKey) {
        let (key, pem) = generated(seed);
        let public = key.pair.public_key().as_ref().to_vec();
        let secret: Vec<u8> = (seed..seed.wrapping_add(32))
            .chain(public.clone())
            .collect();
        assert!(pem.contains(BEGIN));
        let public: &'static [u8] = Box::leak(public.into_boxed_slice());
        (
            File {
                cipher: b"none",
                kdf: b"none",
                keys: 1,
                check_two: 7,
                kind: b"ssh-ed25519",
                public,
                secret,
                padding: Vec::new(),
                extra: b"",
            },
            key,
        )
    }

    fn pem(&self) -> String {
        let mut blob = Vec::new();
        put_string(&mut blob, b"ssh-ed25519");
        put_string(&mut blob, self.public);
        let mut private = Vec::new();
        put_u32(&mut private, 7);
        put_u32(&mut private, self.check_two);
        put_string(&mut private, self.kind);
        put_string(&mut private, self.public);
        put_string(&mut private, &self.secret);
        put_string(&mut private, b"c");
        private.extend_from_slice(&self.padding);
        if self.padding.is_empty() {
            let mut pad = 1u8;
            while !private.len().is_multiple_of(8) {
                private.push(pad);
                pad += 1;
            }
        }
        let mut file = MAGIC.to_vec();
        put_string(&mut file, self.cipher);
        put_string(&mut file, self.kdf);
        put_string(&mut file, b"");
        put_u32(&mut file, self.keys);
        put_string(&mut file, &blob);
        put_string(&mut file, &private);
        file.extend_from_slice(self.extra);
        format!("{BEGIN}\n{}\n{END}\n", STANDARD.encode(file))
    }
}

/// Proves: a key made here is a private key file of `ssh-keygen`'s shape (the armor lines, the
/// base64 in lines of 70, the magic, no cipher, one key, the public key line the public half of
/// it) that reads back as the same key; its public line is `ssh-ed25519` and the blob, with no
/// comment, and its fingerprint is `SHA256:` and 43 characters.
///
/// Catches: a file `ssh-keygen -s` would not read, a key that reads back as another, and a
/// public line that carries the comment into `TrustedUserCAKeys`.
#[test]
fn a_made_key_is_a_private_key_file_that_reads_back_as_the_same_key() {
    let (key, pem) = generated(1);
    assert!(pem.starts_with(&format!("{BEGIN}\n")) && pem.ends_with(&format!("{END}\n")));
    let body: Vec<&str> = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    assert!(
        body.iter().all(|l| l.len() <= 70) && body[..body.len() - 1].iter().all(|l| l.len() == 70)
    );
    let file = STANDARD.decode(body.concat()).expect("base64");
    assert!(file.starts_with(MAGIC));
    let read = CaKey::parse_secret(&pem).expect("reads back");
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].public_line(), key.public_line());
    assert_eq!(read[0].fingerprint(), key.fingerprint());
    let line = key.public_line();
    assert_eq!(line.split(' ').count(), 2);
    assert!(line.starts_with("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5"));
    assert_eq!(key.fingerprint().len(), "SHA256:".len() + 43);
    assert_eq!(key.fingerprint(), fingerprint(&blob_of(&line)));
    assert_eq!(format!("{key:?}"), format!("CaKey({})", key.fingerprint()));
    assert_ne!(generated(2).0.public_line(), line);
}

/// Proves: a CA secret holds one key, or two with `current` first, one after the other, and
/// takes blank lines between them; no key, a third, or any text outside the files is refused.
///
/// Catches: a rotation that reads the keys in the wrong order or ignores a third, and a secret
/// whose trailing text is silently dropped.
#[test]
fn a_ca_secret_is_one_key_or_two_in_order_and_nothing_else() {
    let (first, one) = generated(1);
    let (second, two) = generated(50);
    let (_, three) = generated(90);
    let both = CaKey::parse_secret(&format!("{one}\n\n{two}\n")).expect("two keys");
    assert_eq!(
        both.iter().map(CaKey::public_line).collect::<Vec<_>>(),
        [first.public_line(), second.public_line()]
    );
    assert_eq!(MAX_CA_KEYS, 2);
    for (name, text) in [
        ("empty", String::new()),
        ("blank", "\n \n".to_owned()),
        ("three", format!("{one}{two}{three}")),
        ("text before", format!("a note\n{one}")),
        ("text after", format!("{one}and then\n")),
        ("no end", one.replace(END, "")),
        ("not base64", format!("{BEGIN}\n!!!\n{END}\n")),
    ] {
        assert_eq!(
            CaKey::parse_secret(&text).err(),
            Some(KeyError::BadPrivateKey),
            "{name}"
        );
    }
}

/// Proves: a private key file that is not one unencrypted Ed25519 key whose parts agree is
/// refused, each part by itself: the magic, the cipher, the kdf, the key count, the check numbers,
/// the key type, the public key and the secret's second half, the padding, and bytes after the
/// private part; the well formed file made the same way is accepted.
///
/// Catches: an encrypted key read as a plain one, a file whose public half is another key's (a
/// CA whose `TrustedUserCAKeys` line is not the key that signs), and a damaged file that signs.
#[test]
fn a_private_key_file_is_refused_part_by_part() {
    let (good, key) = File::of(3);
    let read = CaKey::parse_secret(&good.pem()).expect("the good file");
    assert_eq!(read[0].public_line(), key.public_line());
    let mut cases: Vec<(&str, File<'_>)> = Vec::new();
    let broken = |change: &dyn Fn(&mut File<'_>)| {
        let (mut file, _) = File::of(3);
        change(&mut file);
        file
    };
    cases.push(("cipher", broken(&|f| f.cipher = b"aes256-ctr")));
    cases.push(("kdf", broken(&|f| f.kdf = b"bcrypt")));
    cases.push(("two keys", broken(&|f| f.keys = 2)));
    cases.push(("checks", broken(&|f| f.check_two = 8)));
    cases.push(("type", broken(&|f| f.kind = b"ssh-rsa")));
    cases.push(("short secret", broken(&|f| f.secret.truncate(63))));
    cases.push(("tail", broken(&|f| f.secret[40] ^= 1)));
    cases.push(("padding", broken(&|f| f.padding = vec![1, 2, 4])));
    cases.push(("long padding", broken(&|f| f.padding = (1..=8).collect())));
    cases.push(("after", broken(&|f| f.extra = b"x")));
    let (other, _) = File::of(9);
    cases.push(("another public key", broken(&|f| f.public = other.public)));
    for (name, file) in cases {
        assert_eq!(
            CaKey::parse_secret(&file.pem()).err(),
            Some(KeyError::BadPrivateKey),
            "{name}"
        );
    }
    let mut magic = MAGIC.to_vec();
    magic[0] = b'x';
    let wrong = format!("{BEGIN}\n{}\n{END}\n", STANDARD.encode(magic));
    assert_eq!(
        CaKey::parse_secret(&wrong).err(),
        Some(KeyError::BadPrivateKey)
    );
    assert_eq!(
        CaKey::from_seed(&[0; 31]).err(),
        Some(KeyError::BadPrivateKey)
    );
}

/// Proves: a key that cannot be made without random bytes is refused `Crypto`, whichever of the
/// two draws (the seed, the check number) fails.
///
/// Catches: a key made from zeros when the system gave nothing.
#[test]
fn a_key_is_not_made_without_random_bytes() {
    assert_eq!(CaKey::generate("c", &nothing).err(), Some(KeyError::Crypto));
    let once = Cell::new(false);
    let second_fails = |buffer: &mut [u8]| {
        let fails = once.replace(true);
        buffer.fill(1);
        !fails
    };
    assert_eq!(
        CaKey::generate("c", &second_fails).err(),
        Some(KeyError::Crypto)
    );
}

/// Proves: the public key lines `ssh-keygen` writes for Ed25519, ECDSA P-256 and RSA 3072 read, with
/// the fingerprint `ssh-keygen -l` printed for each; a trailing comment or none is the same key.
///
/// Catches: a fingerprint that is not OpenSSH's (an audit line that matches no `ssh-keygen -l`).
#[test]
fn the_keys_ssh_keygen_makes_read_with_its_fingerprints() {
    for (line, print) in [
        (ED25519_KEY, ED25519_PRINT),
        (ECDSA_KEY, ECDSA_PRINT),
        (RSA_KEY, RSA_PRINT),
    ] {
        let parsed = UserKey::parse(line).expect("a key");
        assert_eq!(parsed.fingerprint, print);
        let bare = line.rsplit_once(' ').expect("a comment").0;
        assert_eq!(UserKey::parse(bare).expect("a key"), parsed);
    }
    assert!(matches!(
        UserKey::parse(ED25519_KEY).unwrap().key,
        UserKey::Ed25519 { .. }
    ));
    assert!(matches!(
        UserKey::parse(ECDSA_KEY).unwrap().key,
        UserKey::Ecdsa { .. }
    ));
    assert!(matches!(
        UserKey::parse(RSA_KEY).unwrap().key,
        UserKey::Rsa { .. }
    ));
}

/// Proves: a public key the CA does not sign is `BadPublicKey`: a curve other than P-256, an RSA
/// key of 2048 bits, a type the CA does not know, a line with options before the type or a second
/// line, a type that is not the blob's, a blob with bytes after the key or damaged, an Ed25519 key
/// of the wrong length, an uncompressed point that is not one, and an RSA integer that is negative
/// or not in its shortest form; and a line over 4 KiB.
///
/// Catches: a certificate for a weak or malformed key, and a line with options that smuggles
/// `command=` in front of a key.
#[test]
fn a_public_key_the_ca_does_not_sign_is_refused() {
    let ed = blob_of(ED25519_KEY);
    let ec = blob_of(ECDSA_KEY);
    let rsa = blob_of(RSA_KEY);
    let mut trailing = ed.clone();
    trailing.push(0);
    let mut short = Vec::new();
    put_string(&mut short, b"ssh-ed25519");
    put_string(&mut short, &[1; 31]);
    let mut curve = Vec::new();
    put_string(&mut curve, b"ecdsa-sha2-nistp256");
    put_string(&mut curve, b"nistp384");
    put_string(&mut curve, &[4; 65]);
    let mut compressed = Vec::new();
    put_string(&mut compressed, b"ecdsa-sha2-nistp256");
    put_string(&mut compressed, b"nistp256");
    put_string(&mut compressed, &[2; 65]);
    let mut shorter = Vec::new();
    put_string(&mut shorter, b"ecdsa-sha2-nistp256");
    put_string(&mut shorter, b"nistp256");
    put_string(&mut shorter, &[4; 33]);
    let rsa_with = |e: &[u8], n: &[u8]| {
        let mut blob = Vec::new();
        put_string(&mut blob, b"ssh-rsa");
        put_string(&mut blob, e);
        put_string(&mut blob, n);
        line_of("ssh-rsa", &blob)
    };
    let mut big = vec![0x7f];
    big.extend([1u8; 400]);
    let mut negative = vec![0x80];
    negative.extend([1u8; 400]);
    let mut padded = vec![0, 0x01];
    padded.extend([1u8; 400]);
    let cases = [
        ("p-384", ECDSA_384_KEY.to_owned()),
        ("rsa 2048", RSA_2048_KEY.to_owned()),
        ("rsa e empty", rsa_with(b"", &negative)),
        ("rsa negative", rsa_with(&[1, 0, 1], &negative)),
        ("rsa padded", rsa_with(&[1, 0, 1], &padded)),
        ("rsa too small", rsa_with(&[1, 0, 1], &big[..380])),
        ("dss", line_of("ssh-dss", &ed)),
        ("options", format!("command=\"x\" {ED25519_KEY}")),
        ("two lines", format!("{ED25519_KEY}\n{ED25519_KEY}")),
        ("cr", format!("{ED25519_KEY}\r")),
        ("mismatch", line_of("ssh-ed25519", &ec)),
        ("type of another", line_of("ecdsa-sha2-nistp256", &ed)),
        ("trailing", line_of("ssh-ed25519", &trailing)),
        ("short ed", line_of("ssh-ed25519", &short)),
        ("curve", line_of("ecdsa-sha2-nistp256", &curve)),
        ("compressed", line_of("ecdsa-sha2-nistp256", &compressed)),
        ("short point", line_of("ecdsa-sha2-nistp256", &shorter)),
        ("rsa cut", line_of("ssh-rsa", &rsa[..rsa.len() - 1])),
        ("not base64", "ssh-ed25519 !!!!".to_owned()),
        ("no blob", "ssh-ed25519".to_owned()),
        ("empty", String::new()),
        ("long", format!("{ED25519_KEY} {}", "c".repeat(4096))),
    ];
    for (name, line) in cases {
        assert_eq!(
            UserKey::parse(&line).err(),
            Some(KeyError::BadPublicKey),
            "{name}"
        );
    }
}

/// What a test reads out of a certificate line: every field, as PROTOCOL.certkeys orders them.
pub(crate) struct Cert {
    pub(crate) kind: String,
    pub(crate) nonce: Vec<u8>,
    pub(crate) key: Vec<Vec<u8>>,
    pub(crate) serial: u64,
    pub(crate) cert_type: u32,
    pub(crate) key_id: String,
    pub(crate) principals: Vec<String>,
    pub(crate) valid_after: u64,
    pub(crate) valid_before: u64,
    pub(crate) critical: Vec<u8>,
    pub(crate) extensions: Vec<(String, Vec<u8>)>,
    pub(crate) reserved: Vec<u8>,
    pub(crate) ca: Vec<u8>,
    /// The 64 bytes of the Ed25519 signature the certificate ends with.
    pub(crate) raw_signature: Vec<u8>,
    pub(crate) signed: Vec<u8>,
}

fn u64_of(read: &mut Reader<'_>) -> u64 {
    let high = u64::from(read.u32().expect("a number"));
    (high << 32) | u64::from(read.u32().expect("a number"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("text")
}

pub(crate) fn read_cert(line: &str, key_fields: usize) -> Cert {
    let (kind, body) = line.split_once(' ').expect("two fields");
    let bytes = STANDARD.decode(body).expect("base64");
    let mut read = Reader(&bytes);
    let inner = text(read.string().expect("the type"));
    assert_eq!(inner, kind);
    let nonce = read.string().expect("a nonce").to_vec();
    let key = (0..key_fields)
        .map(|_| read.string().expect("a key field").to_vec())
        .collect();
    let serial = u64_of(&mut read);
    let cert_type = read.u32().expect("a type");
    let key_id = text(read.string().expect("a key id"));
    let mut packed = Reader(read.string().expect("principals"));
    let mut principals = Vec::new();
    while !packed.done() {
        principals.push(text(packed.string().expect("a principal")));
    }
    let valid_after = u64_of(&mut read);
    let valid_before = u64_of(&mut read);
    let critical = read.string().expect("critical options").to_vec();
    let mut packed = Reader(read.string().expect("extensions"));
    let mut extensions = Vec::new();
    while !packed.done() {
        let name = text(packed.string().expect("a name"));
        extensions.push((name, packed.string().expect("data").to_vec()));
    }
    let reserved = read.string().expect("reserved").to_vec();
    let ca = read.string().expect("the CA").to_vec();
    let before = bytes.len() - read.0.len();
    let signature = read.string().expect("a signature").to_vec();
    assert!(read.done());
    let mut inner_signature = Reader(&signature);
    assert_eq!(inner_signature.string(), Some(&b"ssh-ed25519"[..]));
    let raw_signature = inner_signature.string().expect("the signature").to_vec();
    assert!(inner_signature.done());
    Cert {
        kind: inner,
        nonce,
        key,
        serial,
        cert_type,
        key_id,
        principals,
        valid_after,
        valid_before,
        critical,
        extensions,
        reserved,
        ca,
        raw_signature,
        signed: bytes[..before].to_vec(),
    }
}

fn spec<'a>(key: &'a UserKey, principal: &'a str) -> Spec<'a> {
    Spec {
        key,
        serial: 0x0102_0304_0506_0708,
        key_id: "alice/16",
        principal,
        valid_after: 1_800_000_000 - 60,
        valid_before: 1_800_003_601,
    }
}

/// Proves: a certificate for each key type is, field by field, what the CA was told: the
/// certificate type of the key, a 32 byte nonce, the user's key fields as they were, the serial,
/// type 1 (user), the key id, exactly the one principal, the validity, no critical option, the one
/// extension `permit-pty` with no data, nothing reserved, the CA's own public key blob, and
/// an Ed25519 signature that verifies over everything before it with the CA's public key.
///
/// Catches: an agent, port, X11 or `user-rc` permission in a certificate, a second principal, a
/// signature over other bytes than the certificate's, and an RSA or ECDSA key copied wrong into
/// the certificate.
#[test]
fn a_certificate_says_exactly_what_it_was_given_and_is_signed_by_the_ca() {
    let (ca, _) = generated(5);
    for (line, kind, fields) in [
        (ED25519_KEY, "ssh-ed25519-cert-v01@openssh.com", 1),
        (ECDSA_KEY, "ecdsa-sha2-nistp256-cert-v01@openssh.com", 2),
        (RSA_KEY, "ssh-rsa-cert-v01@openssh.com", 2),
    ] {
        let user = UserKey::parse(line).expect("a key");
        let made = ca
            .sign_user(&spec(&user.key, "hk-host-node-07"), &counting(200))
            .expect("signed");
        let cert = read_cert(&made, fields);
        assert_eq!(cert.kind, kind);
        assert!(made.starts_with(&format!("{kind} ")));
        assert_eq!(cert.nonce, (200..232).collect::<Vec<u8>>());
        assert_eq!(cert.serial, 0x0102_0304_0506_0708);
        assert_eq!(cert.cert_type, 1);
        assert_eq!(cert.key_id, "alice/16");
        assert_eq!(cert.principals, ["hk-host-node-07"]);
        assert_eq!(
            (cert.valid_after, cert.valid_before),
            (1_799_999_940, 1_800_003_601)
        );
        assert!(cert.critical.is_empty() && cert.reserved.is_empty());
        assert_eq!(cert.extensions, [("permit-pty".to_owned(), Vec::new())]);
        let user_blob = blob_of(line);
        let mut original = Reader(&user_blob);
        original.string().expect("the type");
        let expected: Vec<Vec<u8>> = (0..fields)
            .map(|_| original.string().expect("a field").to_vec())
            .collect();
        assert_eq!(cert.key, expected);
        assert_eq!(cert.ca, ca.blob());
        let raw = cert.raw_signature.clone();
        UnparsedPublicKey::new(&ED25519, ca.pair.public_key().as_ref())
            .verify(&cert.signed, &raw)
            .expect("the CA's signature verifies");
        let mut wrong = cert.signed.clone();
        wrong[40] ^= 1;
        assert!(
            UnparsedPublicKey::new(&ED25519, ca.pair.public_key().as_ref())
                .verify(&wrong, &raw)
                .is_err()
        );
    }
}

/// Proves: no random bytes, no certificate; and two certificates for one key and one spec differ
/// in their nonce only.
///
/// Catches: a certificate with a fixed nonce, and one signed without the system's randomness.
#[test]
fn a_certificate_has_a_fresh_nonce_and_needs_random_bytes() {
    let (ca, _) = generated(5);
    let user = UserKey::parse(ED25519_KEY).expect("a key");
    assert_eq!(
        ca.sign_user(&spec(&user.key, "p"), &nothing).err(),
        Some(KeyError::Crypto)
    );
    let a = read_cert(
        &ca.sign_user(&spec(&user.key, "p"), &counting(1)).unwrap(),
        1,
    );
    let b = read_cert(
        &ca.sign_user(&spec(&user.key, "p"), &counting(2)).unwrap(),
        1,
    );
    assert_ne!(a.nonce, b.nonce);
    assert_eq!((a.serial, a.key_id), (b.serial, b.key_id));
}
