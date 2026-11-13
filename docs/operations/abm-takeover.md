# Taking over NanoDEP's ABM identity

The coordinator keeps the ABM server entry's key pair and token, so that `js-mdm` in Apple Business Manager needs no change and no device moves
(`docs/design/coordinator-mdm.md`, sections 4.5 and 7). This page is the one-off step where the old MDM stack keeps its files, and what the
operator then does in the setup page. Nothing here changes ABM, NanoDEP or the old stack.

## What NanoDEP keeps

NanoDEP does not keep the `.p7m` file ABM hands over. It keeps the token already decrypted, one file per part, in its `dep_names` directory
(on the farm, `/var/lib/hostkeeper-mdm/dep/dep_names/`), with the server entry's name as the suffix (`js-mdm`):

| File | Holds |
|---|---|
| `consumer_key.js-mdm`, `consumer_secret.js-mdm`, `access_token.js-mdm`, `access_secret.js-mdm` | the four OAuth parts |
| `access_token_expiry.js-mdm` | when the access secret expires (RFC 3339) |
| `key.js-mdm` | the RSA private key of the entry, PKCS#1 PEM (`BEGIN RSA PRIVATE KEY`) |
| `cert.js-mdm` | the entry's self-signed certificate (its validity does not matter to the token) |

## The steps

1. Where those files can be read (the old stack's machine, with a checkout of this repository and `cargo`; the files are not copied elsewhere), as the user that can read them, assemble the token's JSON:

   ```
   cargo xtask abm-takeover-token --dep-dir /var/lib/hostkeeper-mdm/dep/dep_names --name js-mdm --out <file>
   ```

   The verb reads the five files, writes the JSON object `Mdm.PutAbmToken` takes (`consumer_key`, `consumer_secret`, `access_token`, `access_secret`,
   `access_secret_expiry`) to `<file>` with mode 0600, and refuses to overwrite a file that exists. It prints the file's name and no value, and sends
   nothing anywhere. A part that is missing or empty is refused, naming the file.
2. In the setup page, signed in as a user (an API token is refused), choose `key.js-mdm` in the field *Apple Business Manager private key* and `cert.js-mdm` in *Apple Business Manager certificate*:
   the coordinator checks that the key is an RSA key and that the certificate is the key's, and stores them together (kind `abm_key` with its certificate).
3. Choose `<file>` in the field *Apple Business Manager token* (the page sends it as `Mdm.PutAbmToken`; Save may send the key pair and the token in one go, the key pair first). The coordinator opens a session with it and reads the account before it keeps the token, so
   a token that is not good is refused and stores nothing: `abm_credential_refused` (the parts do not belong together, or the token was revoked) and
   `abm_clock_refused` (the clock of the machine the coordinator runs on is an hour or more off) are told apart. The answer names the server entry and
   the organisation Apple says the token belongs to.
4. Remove `<file>`: it holds the token.

The sync then runs every `abm_sync_s` seconds (300). It writes to ABM only after the MDM name is set (the ADE profile is defined and assigned to devices
that have none); until then it only reads the device list.

## When the key pair cannot be read out

Download the coordinator's own public key instead (the link *Download the public certificate to upload to ABM* next to the token field, `Mdm.GetAbmPublicKey`: it makes an RSA 2048 key pair once and answers the certificate), add the
certificate to the server entry in ABM as the MDM server's key, and upload the token file ABM then returns (`.p7m`) with `Mdm.PutAbmToken` as
`token_p7m`. The coordinator opens it with its own key. Whether ABM accepts a replaced key for an existing entry is not verified (design section 12,
item 16).
