//! A stand-in for the release workflow's signer: it makes the Sigstore bundle and the trusted
//! root that the production attestation verifier (`gh attestation verify`, run by the daemon)
//! accepts for a package built by the tests, with no network and no real Sigstore.
//!
//! How `gh` is satisfied, in short (the full chain of reasoning is in the checks of sigstore-go,
//! which `gh` embeds):
//!
//! - The trusted root names one certificate authority (a root made here, standing for Fulcio) and
//!   one timestamp authority (a root and a leaf made here). Its Fulcio root has exactly one
//!   organisation `O`, which is also the only `O` of the leaf's issuer; `gh` picks its verifier by
//!   that name, and a name that is neither Sigstore's nor GitHub's gets the generic verifier, which
//!   asks for one signed timestamp and nothing from a transparency log.
//! - The bundle (v0.3) carries the Fulcio-like leaf, whose subject alternative name is the
//!   workflow's URI and whose Fulcio extensions say who built what, a DSSE envelope over an in-toto
//!   statement for the package, signed by the leaf's key, and an RFC 3161 `TimeStampResp` from the
//!   timestamp authority over that signature. The time stamped is "now" when the signer is made,
//!   and every certificate is valid then, which is the time `gh` verifies the leaf at.
//! - The certificates, the timestamp token and the statement are written byte by byte with the
//!   small DER writer of [`super::der`]; the keys are P-256 and every signature is ECDSA over
//!   SHA-256 in its DER form.
//!
//! The daemon's own pre-checks read two things from the bundle before `gh` runs: the source ref
//! from the leaf's extension `1.3.6.1.4.1.57264.1.14` and the one `gitCommit` among the
//! provenance's `resolvedDependencies`; both are present and agree with the identity constants
//! of the rig (`super::SOURCE_REF`, `super::COMMIT`).
//!
//! Nothing here is secure: the keys are random, held in memory and thrown away with the signer.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{DerSignature, SigningKey};
use p256::pkcs8::EncodePublicKey;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::der::{self, Civil};
use super::{COMMIT, ORGANISATION, REPOSITORY, SOURCE_REF, WORKFLOW};

/// The `O` of both root certificates and the issuer name `gh` selects its verifier by: neither
/// `sigstore.dev` nor `GitHub, Inc.`.
const ISSUER_ORGANISATION: &str = "hostkeeper-fixture";
/// The OIDC issuer of GitHub Actions, the value of the Fulcio extension `1.3.6.1.4.1.57264.1.8`.
const OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
/// The in-toto statement's predicate type, `gh`'s default for `--predicate-type`.
const PREDICATE_TYPE: &str = "https://slsa.dev/provenance/v1";
/// The DSSE payload type of an in-toto statement.
const PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";
/// The trusted root's media type.
const TRUSTED_ROOT_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.trustedroot+json;version=0.1";
/// The bundle's media type.
const BUNDLE_MEDIA_TYPE: &str = "application/vnd.dev.sigstore.bundle.v0.3+json";
/// A timestamp policy: any object identifier is accepted; this is under the fixture's own arc.
const TSA_POLICY: [u64; 9] = [1, 3, 6, 1, 4, 1, 57264, 1, 1000];

/// One day, in seconds.
const DAY: i64 = 86_400;
/// How long before the signer's time every certificate is valid.
const VALID_BEFORE: i64 = DAY;
/// How long after the signer's time every certificate is valid: ten years, so that `UTCTime`
/// (years below 2050) suffices for a long while and a test run never outlives a certificate.
const VALID_AFTER: i64 = 3650 * DAY;
/// How long before the signer's time the trusted root's authorities are valid from.
const ROOT_VALID_BEFORE: i64 = 2 * DAY;

/// The object identifiers used, as arcs.
mod oids {
    /// `ecdsa-with-SHA256`.
    pub const ECDSA_SHA256: [u64; 7] = [1, 2, 840, 10045, 4, 3, 2];
    /// `id-sha256`.
    pub const SHA256: [u64; 9] = [2, 16, 840, 1, 101, 3, 4, 2, 1];
    /// `commonName`.
    pub const COMMON_NAME: [u64; 4] = [2, 5, 4, 3];
    /// `organizationName`.
    pub const ORGANISATION: [u64; 4] = [2, 5, 4, 10];
    /// `basicConstraints`.
    pub const BASIC_CONSTRAINTS: [u64; 4] = [2, 5, 29, 19];
    /// `keyUsage`.
    pub const KEY_USAGE: [u64; 4] = [2, 5, 29, 15];
    /// `extKeyUsage`.
    pub const EXTENDED_KEY_USAGE: [u64; 4] = [2, 5, 29, 37];
    /// `subjectAltName`.
    pub const SUBJECT_ALT_NAME: [u64; 4] = [2, 5, 29, 17];
    /// `id-kp-codeSigning`.
    pub const CODE_SIGNING: [u64; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 3];
    /// `id-kp-timeStamping`.
    pub const TIME_STAMPING: [u64; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 8];
    /// `id-signedData`.
    pub const SIGNED_DATA: [u64; 7] = [1, 2, 840, 113549, 1, 7, 2];
    /// `id-ct-TSTInfo`.
    pub const TST_INFO: [u64; 9] = [1, 2, 840, 113549, 1, 9, 16, 1, 4];
    /// `contentType` (a signed attribute).
    pub const CONTENT_TYPE: [u64; 7] = [1, 2, 840, 113549, 1, 9, 3];
    /// `messageDigest` (a signed attribute).
    pub const MESSAGE_DIGEST: [u64; 7] = [1, 2, 840, 113549, 1, 9, 4];
    /// The arc under which Fulcio puts the facts of a build: `1.3.6.1.4.1.57264.1`.
    pub const FULCIO: [u64; 8] = [1, 3, 6, 1, 4, 1, 57264, 1];
}

/// Keys, certificates and the time of one signer, made fresh by [`FixtureSigner::new`].
pub struct FixtureSigner {
    /// The time every certificate and the timestamp are made for, in unix seconds.
    gen_time: i64,
    /// The Fulcio-like root, DER.
    fulcio_root: Vec<u8>,
    /// The signing leaf, DER, and its key (the DSSE signature is made with it).
    leaf: Vec<u8>,
    leaf_key: SigningKey,
    /// The timestamp authority's root and leaf, DER, and the leaf's key.
    tsa_root: Vec<u8>,
    tsa_leaf: Vec<u8>,
    tsa_key: SigningKey,
    /// The TSA leaf's issuer `Name` (a whole SEQUENCE) and serial `INTEGER` (a whole element),
    /// copied byte for byte into the timestamp's `SignerInfo`.
    tsa_issuer: Vec<u8>,
    tsa_serial: Vec<u8>,
}

impl Default for FixtureSigner {
    fn default() -> FixtureSigner {
        FixtureSigner::new()
    }
}

impl FixtureSigner {
    /// A signer with fresh random keys, valid from a day ago to ten years ahead, whose time is
    /// now.
    pub fn new() -> FixtureSigner {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the unix epoch")
            .as_secs();
        let gen_time = i64::try_from(now).expect("the unix time fits an i64");

        let fulcio_root_key = new_key();
        let fulcio_root_name = name(Some(ISSUER_ORGANISATION), "hostkeeper-fixture-fulcio-root");
        let fulcio_root = issue(
            gen_time,
            &Subject {
                name: &fulcio_root_name,
                key: &fulcio_root_key,
                extensions: ca_extensions(),
            },
            &fulcio_root_name,
            &fulcio_root_key,
        )
        .der;

        let leaf_key = new_key();
        let leaf_name = name(None, "hostkeeper-fixture-signer");
        let leaf = issue(
            gen_time,
            &Subject {
                name: &leaf_name,
                key: &leaf_key,
                extensions: leaf_extensions(),
            },
            &fulcio_root_name,
            &fulcio_root_key,
        )
        .der;

        let tsa_root_key = new_key();
        let tsa_root_name = name(Some(ISSUER_ORGANISATION), "hostkeeper-fixture-tsa-root");
        let tsa_root = issue(
            gen_time,
            &Subject {
                name: &tsa_root_name,
                key: &tsa_root_key,
                extensions: ca_extensions(),
            },
            &tsa_root_name,
            &tsa_root_key,
        )
        .der;

        let tsa_key = new_key();
        let tsa_leaf_name = name(Some(ISSUER_ORGANISATION), "hostkeeper-fixture-tsa");
        let tsa = issue(
            gen_time,
            &Subject {
                name: &tsa_leaf_name,
                key: &tsa_key,
                extensions: tsa_leaf_extensions(),
            },
            &tsa_root_name,
            &tsa_root_key,
        );

        FixtureSigner {
            gen_time,
            fulcio_root,
            leaf,
            leaf_key,
            tsa_root,
            tsa_leaf: tsa.der,
            tsa_key,
            // The issuer of the TSA leaf is the TSA root's subject, and the SignerInfo names it
            // by exactly these bytes.
            tsa_issuer: tsa_root_name,
            tsa_serial: tsa.serial,
        }
    }

    /// The trusted root `gh --custom-trusted-root` reads: one line of compact JSON and a newline
    /// (`gh` reads it line by line and drops a last line without one).
    pub fn trusted_root_jsonl(&self) -> String {
        let start = Civil::from_unix(self.gen_time - ROOT_VALID_BEFORE).rfc3339();
        let b64 = |der: &[u8]| STANDARD.encode(der);
        let root = json!({
            "mediaType": TRUSTED_ROOT_MEDIA_TYPE,
            "certificateAuthorities": [{
                "subject": {
                    "organization": ISSUER_ORGANISATION,
                    "commonName": "hostkeeper-fixture-fulcio-root",
                },
                "uri": "https://fulcio.invalid",
                "certChain": { "certificates": [{ "rawBytes": b64(&self.fulcio_root) }] },
                "validFor": { "start": start },
            }],
            "timestampAuthorities": [{
                "subject": {
                    "organization": ISSUER_ORGANISATION,
                    "commonName": "hostkeeper-fixture-tsa",
                },
                "uri": "https://tsa.invalid",
                "certChain": { "certificates": [
                    { "rawBytes": b64(&self.tsa_leaf) },
                    { "rawBytes": b64(&self.tsa_root) },
                ] },
                "validFor": { "start": start },
            }],
        });
        format!("{root}\n")
    }

    /// The bundle (Sigstore bundle v0.3, JSON) attesting that the package file `subject_name`
    /// has the SHA-256 `sha256_hex` (64 lower-case hex digits).
    pub fn bundle(&self, subject_name: &str, sha256_hex: &str) -> Vec<u8> {
        let statement = statement(subject_name, sha256_hex);
        let signature = sign(&self.leaf_key, &pae(PAYLOAD_TYPE, &statement));
        let timestamp = self.timestamp_response(&signature);
        let bundle = json!({
            "mediaType": BUNDLE_MEDIA_TYPE,
            "verificationMaterial": {
                "certificate": { "rawBytes": STANDARD.encode(&self.leaf) },
                "timestampVerificationData": {
                    "rfc3161Timestamps": [{ "signedTimestamp": STANDARD.encode(&timestamp) }],
                },
            },
            "dsseEnvelope": {
                "payload": STANDARD.encode(&statement),
                "payloadType": PAYLOAD_TYPE,
                "signatures": [{ "sig": STANDARD.encode(&signature) }],
            },
        });
        serde_json::to_vec(&bundle).expect("a bundle is JSON")
    }

    /// The `[trusted_signers]` table of the daemon's config for packages this signer attests.
    pub fn trusted_signers_toml(&self) -> String {
        format!(
            "[trusted_signers]\norganisation = \"{ORGANISATION}\"\nworkflow = \"{WORKFLOW}\"\n\
             refs = [\"{SOURCE_REF}\"]\nhosted_runner = true\n"
        )
    }

    /// The RFC 3161 `TimeStampResp` over `signature`, the raw DER bytes of the DSSE signature: a
    /// granted status and a `SignedData` token whose content is a `TSTInfo` with a SHA-256
    /// imprint of `signature`, signed by the TSA leaf, which is embedded.
    fn timestamp_response(&self, signature: &[u8]) -> Vec<u8> {
        let digest_algorithm = || der::sequence(vec![der::oid(&oids::SHA256), der::null()]);

        // TSTInfo ::= SEQUENCE { version 1, policy, messageImprint, serialNumber, genTime }; the
        // optional accuracy, ordering, nonce, tsa and extensions are left out.
        let tst_info = der::sequence(vec![
            der::small_integer(1),
            der::oid(&TSA_POLICY),
            der::sequence(vec![
                digest_algorithm(),
                der::octet_string(&sha256(signature)),
            ]),
            random_serial(),
            der::generalized_time(self.gen_time),
        ]);

        // The signed attributes, each `SEQUENCE { type, SET { value } }`, sorted by encoding.
        let mut attributes = vec![
            der::sequence(vec![
                der::oid(&oids::CONTENT_TYPE),
                der::set(vec![der::oid(&oids::TST_INFO)]),
            ]),
            der::sequence(vec![
                der::oid(&oids::MESSAGE_DIGEST),
                der::set(vec![der::octet_string(&sha256(&tst_info))]),
            ]),
        ];
        attributes.sort();
        let attributes = attributes.concat();
        // The signature covers the attributes as a SET OF; on the wire they carry the tag [0].
        let signature_input = der::tlv(der::SET, &attributes);
        let wire_attributes = der::tlv(der::CONTEXT | der::CONSTRUCTED, &attributes);

        let signer_info = der::sequence(vec![
            der::small_integer(1),
            // IssuerAndSerialNumber, both copied from the leaf.
            der::sequence(vec![self.tsa_issuer.clone(), self.tsa_serial.clone()]),
            digest_algorithm(),
            wire_attributes,
            der::sequence(vec![der::oid(&oids::ECDSA_SHA256)]),
            der::octet_string(&sign(&self.tsa_key, &signature_input)),
        ]);

        let signed_data = der::sequence(vec![
            der::small_integer(3),
            der::set(vec![digest_algorithm()]),
            der::sequence(vec![
                der::oid(&oids::TST_INFO),
                der::explicit(0, &der::octet_string(&tst_info)),
            ]),
            // certificates [0] IMPLICIT SET OF Certificate: the TSA leaf alone.
            der::tlv(der::CONTEXT | der::CONSTRUCTED, &self.tsa_leaf),
            der::set(vec![signer_info]),
        ]);

        der::sequence(vec![
            // PKIStatusInfo { status granted }.
            der::sequence(vec![der::small_integer(0)]),
            der::sequence(vec![
                der::oid(&oids::SIGNED_DATA),
                der::explicit(0, &signed_data),
            ]),
        ])
    }
}

/// What a certificate is for: its subject name, key and extensions.
struct Subject<'a> {
    /// The subject `Name`, a whole SEQUENCE.
    name: &'a [u8],
    key: &'a SigningKey,
    /// Complete `Extension` elements.
    extensions: Vec<Vec<u8>>,
}

/// A made certificate: its DER and its serial number as a whole `INTEGER` element.
struct Issued {
    der: Vec<u8>,
    serial: Vec<u8>,
}

/// A certificate for `subject` signed by `issuer_key`, whose name is `issuer_name` (a whole `Name`
/// element, copied as the certificate's issuer), valid around `gen_time`.
///
/// ```text
/// Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signatureValue BIT STRING }
/// TBSCertificate ::= SEQUENCE { [0] version v3, serialNumber, signature, issuer, validity,
///                               subject, subjectPublicKeyInfo, [3] extensions }
/// ```
fn issue(
    gen_time: i64,
    subject: &Subject<'_>,
    issuer_name: &[u8],
    issuer_key: &SigningKey,
) -> Issued {
    let serial = random_serial();
    let algorithm = der::sequence(vec![der::oid(&oids::ECDSA_SHA256)]);
    let public_key = subject
        .key
        .verifying_key()
        .to_public_key_der()
        .expect("a P-256 public key encodes");
    let tbs = der::sequence(vec![
        der::explicit(0, &der::small_integer(2)),
        serial.clone(),
        algorithm.clone(),
        issuer_name.to_vec(),
        der::sequence(vec![
            der::time(gen_time - VALID_BEFORE),
            der::time(gen_time + VALID_AFTER),
        ]),
        subject.name.to_vec(),
        public_key.as_bytes().to_vec(),
        der::explicit(3, &der::sequence(subject.extensions.clone())),
    ]);
    let signature = sign(issuer_key, &tbs);
    let der = der::sequence(vec![tbs, algorithm, der::bit_string(0, &signature)]);
    Issued { der, serial }
}

/// A `Name` of one or two relative distinguished names, each with the single attribute given:
/// the organisation (when there is one) and the common name.
fn name(organisation: Option<&str>, common_name: &str) -> Vec<u8> {
    let attribute = |oid: &[u64], value: &str| {
        der::set(vec![der::sequence(vec![
            der::oid(oid),
            der::utf8_string(value),
        ])])
    };
    let mut rdns = Vec::new();
    if let Some(organisation) = organisation {
        rdns.push(attribute(&oids::ORGANISATION, organisation));
    }
    rdns.push(attribute(&oids::COMMON_NAME, common_name));
    der::sequence(rdns)
}

/// One `Extension ::= SEQUENCE { extnID, critical BOOLEAN DEFAULT FALSE, extnValue OCTET STRING }`
/// whose value is the DER `value`. A false `critical` is omitted, as DER requires.
fn extension(id: &[u64], critical: bool, value: &[u8]) -> Vec<u8> {
    let mut parts = vec![der::oid(id)];
    if critical {
        parts.push(der::boolean(true));
    }
    parts.push(der::octet_string(value));
    der::sequence(parts)
}

/// `keyUsage` (critical) with the named bits set: bit 0 is `digitalSignature`, 5 `keyCertSign`
/// and 6 `cRLSign`. DER drops the trailing zero bits of a named-bit string, which the unused-bit
/// count of the one octet says.
fn key_usage(bits: &[u8]) -> Vec<u8> {
    let octet = bits.iter().fold(0_u8, |octet, &bit| octet | (0x80 >> bit));
    assert_ne!(octet, 0, "a key usage names at least one bit");
    let unused = u8::try_from(octet.trailing_zeros()).expect("at most eight unused bits");
    extension(&oids::KEY_USAGE, true, &der::bit_string(unused, &[octet]))
}

/// `extKeyUsage` with the one purpose `purpose`.
fn extended_key_usage(purpose: &[u64], critical: bool) -> Vec<u8> {
    extension(
        &oids::EXTENDED_KEY_USAGE,
        critical,
        &der::sequence(vec![der::oid(purpose)]),
    )
}

/// The extensions of a root: `basicConstraints` CA (critical, no path length) and `keyUsage`
/// `keyCertSign | cRLSign` (critical).
fn ca_extensions() -> Vec<Vec<u8>> {
    vec![
        extension(
            &oids::BASIC_CONSTRAINTS,
            true,
            &der::sequence(vec![der::boolean(true)]),
        ),
        key_usage(&[5, 6]),
    ]
}

/// The URI of the workflow run's identity: the certificate's subject alternative name and the
/// Fulcio extensions `.9` and `.18`.
fn workflow_uri() -> String {
    format!("https://github.com/{ORGANISATION}/{REPOSITORY}/{WORKFLOW}@{SOURCE_REF}")
}

/// A Fulcio extension `1.3.6.1.4.1.57264.1.<arc>` (non-critical) holding a DER `UTF8String`.
fn fulcio(arc: u64, value: &str) -> Vec<u8> {
    let mut id = oids::FULCIO.to_vec();
    id.push(arc);
    extension(&id, false, &der::utf8_string(value))
}

/// The extensions of the signing leaf: `keyUsage` `digitalSignature`, `extKeyUsage` code signing,
/// the workflow URI as the one `uniformResourceIdentifier` name, and the Fulcio facts of a
/// GitHub-hosted build of `COMMIT` at `SOURCE_REF`. None but `keyUsage` is critical.
fn leaf_extensions() -> Vec<Vec<u8>> {
    let uri = workflow_uri();
    let repository = format!("https://github.com/{ORGANISATION}/{REPOSITORY}");
    let owner = format!("https://github.com/{ORGANISATION}");
    vec![
        key_usage(&[0]),
        extended_key_usage(&oids::CODE_SIGNING, false),
        extension(
            &oids::SUBJECT_ALT_NAME,
            false,
            // GeneralNames ::= SEQUENCE OF GeneralName; uniformResourceIdentifier is [6] IA5String.
            &der::sequence(vec![der::implicit(6, uri.as_bytes())]),
        ),
        fulcio(8, OIDC_ISSUER),
        fulcio(9, &uri),
        fulcio(11, "github-hosted"),
        fulcio(12, &repository),
        fulcio(13, COMMIT),
        fulcio(14, SOURCE_REF),
        fulcio(16, &owner),
        fulcio(18, &uri),
    ]
}

/// The extensions of the timestamp authority's leaf: `keyUsage` `digitalSignature` and the one
/// purpose `timeStamping`, which the verifier requires to be critical. It is not a CA, so it has
/// no `basicConstraints`.
fn tsa_leaf_extensions() -> Vec<Vec<u8>> {
    vec![
        key_usage(&[0]),
        extended_key_usage(&oids::TIME_STAMPING, true),
    ]
}

/// The in-toto statement for the package, as the bytes the DSSE envelope carries. Its predicate
/// is the shape of GitHub's SLSA provenance, as far as the daemon reads it: one
/// `resolvedDependencies` entry whose `digest.gitCommit` is the commit.
fn statement(subject_name: &str, sha256_hex: &str) -> Vec<u8> {
    let uri = workflow_uri();
    let statement = json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{ "name": subject_name, "digest": { "sha256": sha256_hex } }],
        "predicateType": PREDICATE_TYPE,
        "predicate": {
            "buildDefinition": {
                "buildType": "https://actions.github.io/buildtypes/workflow/v1",
                "externalParameters": {
                    "workflow": {
                        "ref": SOURCE_REF,
                        "repository": format!("https://github.com/{ORGANISATION}/{REPOSITORY}"),
                        "path": WORKFLOW,
                    },
                },
                "internalParameters": {},
                "resolvedDependencies": [{
                    "uri": format!(
                        "git+https://github.com/{ORGANISATION}/{REPOSITORY}@{SOURCE_REF}"
                    ),
                    "digest": { "gitCommit": COMMIT },
                }],
            },
            "runDetails": { "builder": { "id": uri } },
        },
    });
    serde_json::to_vec(&statement).expect("a statement is JSON")
}

/// The DSSE pre-authentication encoding: `"DSSEv1" SP len(type) SP type SP len(body) SP body`,
/// with the lengths in ASCII decimal.
fn pae(payload_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "DSSEv1 {} {payload_type} {} ",
        payload_type.len(),
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// A fresh P-256 key from the operating system's randomness.
fn new_key() -> SigningKey {
    loop {
        // A random 32-byte string is almost always a valid scalar; draw again when it is not.
        if let Ok(key) = SigningKey::from_slice(&random::<32>()) {
            return key;
        }
    }
}

/// `N` random bytes.
fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).expect("the operating system has randomness");
    bytes
}

/// A certificate or timestamp serial number as a whole `INTEGER` element: 16 random octets with
/// the top bit cleared and a non-zero first octet, so it is positive and 16 octets long.
fn random_serial() -> Vec<u8> {
    loop {
        let mut bytes = random::<16>();
        bytes[0] &= 0x7f;
        if bytes[0] != 0 {
            return der::integer(&bytes);
        }
    }
}

/// ECDSA over SHA-256 of `message`, as the DER `SEQUENCE { r, s }`.
fn sign(key: &SigningKey, message: &[u8]) -> Vec<u8> {
    let signature: DerSignature = key.sign(message);
    signature.as_bytes().to_vec()
}

/// The SHA-256 of `bytes`.
fn sha256(bytes: &[u8]) -> Vec<u8> {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest.to_vec()
}
