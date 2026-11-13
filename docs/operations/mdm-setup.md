# MDM setup: where each file and value comes from

The MDM setup page of the web app is one form. Each field takes one file or one value, and each has a
*How do I get this?* link to the section of this page that says where it comes from. Save sends
whatever fields are filled; an empty file field keeps what the coordinator already holds. The page's
own description is in `docs/ui-serving.md`; the cutover from the old stack, which has the paths of the
files on the old box, is `docs/operations/mdm-cutover.md`.

What the form does not ask for: the MDM name (shown read-only in the status line; it is the coordinator's `--mdm-host`,
which the coordinator stores as the name at start, so the form asks for it only when the coordinator was started
without `--mdm-host` and none is stored. A different `--mdm-host` replaces the stored name at the next start while
no Mac has enrolled, and is refused at start once one has: the name cannot change after enrollment), the DNS zone (worked out from the MDM name: `mdm.internal.example.net` is ordered in
`example.net`; a zone already set through the API is kept), the ACME directory (production Let's Encrypt
unless the setting `acme_directory` says another, which is an API change) and the DNS provider (GoDaddy).

Files are read from your own machine when Save is pressed, sent and dropped; the page never shows a key
or a token again. Keep your copies in a directory only you can read, with mode 0600, and remove them with
the scratch removal tool when the upload is done.

## Push certificate

Two files, uploaded together: the certificate (PEM) and its private key (PEM, RSA, PKCS#1 or PKCS#8). The
coordinator refuses the pair when the key is not the certificate's, when the certificate has expired, and
when its topic does not start with `com.apple.mgmt.External.`; for a renewal the topic must be the one in use,
because every enrolled Mac carries it. The refusal is shown under the key or the certificate, whichever it is about.

**You already have one** (the cutover): it is the pair NanoHUB holds; the paths are in
`docs/operations/mdm-cutover.md`, step (d). Upload both files.

**You need a new one.** Apple signs a push certificate only for a request signed by an MDM vendor.
[mdmcert.download](https://mdmcert.download) signs one for you:

1. Make the key and the request on your machine: `openssl req -new -newkey rsa:2048 -nodes -keyout push.key -out push.csr -subj "/CN=hostkeeper MDM push"`.
   Keep `push.key`: it is the private key to upload.
2. Give `push.csr` to mdmcert.download as its own instructions say. It emails the signed request, encrypted to a
   certificate of the request you made with it; decrypt it as its instructions say.
3. Upload the signed request at the Apple Push Certificates Portal (`identity.apple.com/pushcert`), signed in with the
   Apple ID that is to own the certificate (use a shared one, not a person's: the certificate is renewed every year
   with the same Apple ID), and download the certificate Apple returns (`.pem`).
4. Upload that certificate and `push.key` in the form.

## Apple Business Manager token

One file: either the token ABM hands over, a `.p7m`, or an existing token as JSON. The page tells them apart by
content (JSON begins with `{`); a `.p7m` is opened with the ABM key, so the ABM key and certificate fields must
hold the key it was made for (stored already, or uploaded in the same Save). The coordinator proves the token to
Apple before it keeps it; a token that is not good is refused and nothing is stored.

- **A new `.p7m`**: in Apple Business Manager, Preferences, *Your MDM Servers*, choose the server entry and
  download its token. ABM encrypts it to the public key of that entry, so it opens only with that key (next section).
- **An existing token held decrypted** (the old stack keeps the four parts of the token in separate files): the
  `xtask` verb of `docs/operations/abm-takeover.md` writes the JSON the page takes. The JSON is an object of
  `consumer_key`, `consumer_secret`, `access_token`, `access_secret` and `access_secret_expiry`.

The token expires; the field shows `stored ✓ (expires YYYY-MM-DD)` and the setting `abm_alert_days` says when
the coordinator warns.

## Apple Business Manager key pair

Two files, the private key and the certificate of the ABM server entry, both PEM. They are stored together
(the key is kept only when it is the certificate's), or either alone when the other is held.

- **Taking over an existing server entry** (no change in ABM): the key pair is the one the entry was made with. On
  the old stack it is NanoDEP's `key.<server>` and `cert.<server>`; paths in `docs/operations/mdm-cutover.md`,
  step (d), and the checks in `docs/operations/abm-takeover.md`.
- **A new server entry**: leave both fields empty and use the link *Download the public certificate to upload to
  ABM* next to the token field (shown only while no ABM key is stored). The coordinator makes the key pair at the
  first click and keeps it; add the certificate it gives as the public key of the MDM server in ABM, download the
  token ABM then makes, and upload that `.p7m` in the token field. Whether ABM accepts a replaced key for an
  existing entry is not verified (`docs/design/coordinator-mdm.md`, section 12, item 16).

## GoDaddy API key

Two values typed into the form, the key and the secret. They let the coordinator add and remove the DNS-01
challenge record of Let's Encrypt in the zone of the MDM name, to get and renew the TLS certificate of the MDM
name. They are stored sealed and never shown again.

1. Sign in at [developer.godaddy.com](https://developer.godaddy.com) with the GoDaddy account that holds the domain
   of the MDM name, open *API Keys* and create a key in the **Production** environment. Test keys do not reach real
   domains.
2. GoDaddy shows the secret once. Copy the key and the secret into the form.

The zone it orders in is the registrable domain of the MDM name, set once (when the setting `dns_zone` is empty);
use the API to set another. The contact email for Let's Encrypt is where it writes about the certificate; the form
offers the stored address, else your sign-in name when that is an address.
