# Enrolling a node

How one server becomes a host the coordinator manages: the operator records it in the host
inventory, the node boots and asks for a certificate, and the operator checks the request and
approves it. The coordinator's side (its flags, its CA, its state files) is
[enrollment-coordinator.md](../enrollment-coordinator.md); every command, flag and exit code of
`hostkeeper ctl` is [ctl.md](../ctl.md).

Every `hostkeeper ctl enrollment` call takes the operator token only: a CI token is refused with
`forbidden`, no token with `unauthorized`, and a refused call changes nothing. An approval and
every change to a record is an entry of the audit log, under the name of the token that made it.
The commands below reach the coordinator's private listener (`--private-listen`):
`https://<private-listener>` is its address, `<ca.pem>` the CA its certificate chains to, and
`<token-file>` a file holding an operator token. A refusal prints `refused: <token>` on stderr;
the tokens are in [Refusals of the operator calls](#refusals-of-the-operator-calls).

## Record the server

Before the server first boots, add a record of it: the host id it will have, its role, the MACs
of its data interfaces, the MAC of its BMC, its serial and a note. A record needs at least one
data MAC, or a serial; it holds at most 16 data MACs, and none of them may be the BMC MAC. MACs
are written as six pairs of hex digits separated all by `:` or all by `-`, or as twelve hex
digits, in either case, and are kept as lower-case `aa:bb:cc:dd:ee:ff`.

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment add-record --host-id web-17 --role web \
    --data-macs 00:00:5e:00:53:01 --data-macs 00:00:5e:00:53:02 \
    --bmc-mac 00:00:5e:00:53:a1 --serial SN-0042 --notes "rack 4, unit 17"
```

The answer is the record as kept, with `added_by` (the token's name). A second record of the same
host id is refused `record_exists`; a record with a MAC or a serial that another record holds is
refused `record_conflict`. To change a record, remove it and add it again. Removing a record does
not touch a host already enrolled under it: the host keeps its certificate.

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment list-records --limit 50
```

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment remove-record --host-id web-17 --reason "wrong BMC MAC"
```

`list-records` lists the records by host id, at most 256 a page; when `next_page_token` is set,
pass it as `--page-token` for the next page. Each record says `enrolled: true` when a host is
enrolled against it (the claims of the request the host's key was approved from match this record
and no other, and the host is not denied); `--enrolled true` lists only those records, and
`--enrolled false` only the others, which is the list of servers still to come online. Each
record also says `hostHeld`: what the enrollment store holds for the record's host id, read from
the store, so the same after a coordinator restart and whether or not the host has sent a
heartbeat: `HOST_HELD_NONE` (no host by that id), `HOST_HELD_APPROVED` (approved and not denied),
`HOST_HELD_DENIED` (retired with `hosts retire`, an update still in flight) or
`HOST_HELD_RETIRED` (retired, final). A coordinator without enrollment answers
`HOST_HELD_UNSPECIFIED`. `hostkeeper-pxe` offers a network install only for a record whose
`hostHeld` is `HOST_HELD_NONE` ([network install](../design/network-install.md), section 3), so a
retired host's record keeps its machine from being reinstalled by accident. There is no call that
reads one record by host id: list the records and find it. The coordinator keeps at most 10000 records (`capacity` past them).

## Bootstrap a Linux node

The deb and the rpm are [packaging/linux](../../packaging/linux/README.md). Install one as root:

```
dpkg -i hostkeeper_<version>_amd64.deb
rpm -i hostkeeper-<version>-1.x86_64.rpm
```

(`arm64` and `aarch64` on an Arm host). On a first install the package makes the daemon's
directories and enables `hostkeeper.service`. It does not start it, and it places no config.

Place the config:

1. Copy the template: `cp /usr/share/hostkeeper/hostkeeper.toml.template
   /etc/hostkeeper/hostkeeper.toml`.
2. Replace each of its seven quoted values that holds an `@@NAME@@` placeholder, the quotes and
   the hint included:

   | key | placeholder | value |
   |---|---|---|
   | `coordinator` | `@@COORDINATOR@@` | the coordinator's heartbeat listener (`--heartbeat-listen`), as a quoted `https://` URL, with a port if it is not 443 |
   | `ca_sha256` | `@@CA_SHA256@@` | the SHA-256 of the coordinator's enrollment CA certificate, 64 hex digits, quoted |
   | `release_channel` | `@@RELEASE_CHANNEL@@` | where packages are fetched from, a quoted `https://` URL |
   | `organisation` | `@@SIGNER_ORGANISATION@@` | the GitHub organisation whose releases the host installs, quoted |
   | `workflow` | `@@SIGNER_WORKFLOW@@` | the release workflow, as a quoted path in each repository |
   | `refs` | `@@SIGNER_REFS@@` | one or more quoted ref globs, in the list's brackets |
   | `hosted_runner` | `@@SIGNER_HOSTED_RUNNER@@` | `true` or `false`, without quotes |

   The coordinator logs `ca_sha256` at its start, on the line `the enrollment CA`. It is also
   the `sha256sum` of `<ca-key-file>.cert`, the coordinator's CA certificate, which is DER. It is
   not the hash of `--server-cert-file`.
3. Check that no placeholder is left: `grep -n '@@' /etc/hostkeeper/hostkeeper.toml` prints
   nothing.
4. Make it root's and closed to writes by others: `chown root:root /etc/hostkeeper/hostkeeper.toml`
   and `chmod 0600 /etc/hostkeeper/hostkeeper.toml`.

Start the unit by hand, since the package does not: `systemctl start hostkeeper.service`. The unit
is `Type=notify`, so the command returns once the daemon has finished its boot work. Its lines go
to the journal (`journalctl -u hostkeeper.service`), each beginning `hostkeeper daemon: `:

- `not enrolled; waiting for approval`: the request is posted and waits for an operator. The
  enrollment loop asks again every 5 seconds and writes a line only when it changes.
- `not enrolled: waiting for /etc/updater/pki/current`: the heartbeat loop has no certificate yet
  and sends nothing.
- `/etc/hostkeeper/hostkeeper.toml: the daemon config's <key> <why>`: the config does not load
  (exit 3); nothing on the host changed. The loader reports one key at a time. systemd keeps
  starting the daemon again, so once the file is fixed the next start loads it; nothing needs
  resetting.
- `the coordinator presents a CA whose SHA-256 is <hex>, not the pinned ca_sha256; nothing was
  sent`: `ca_sha256` or `coordinator` is wrong.
- `the coordinator could not be reached: Enroll was answered with the status <code>: "<token>"`:
  the coordinator refused the request; the token is in
  [Refusals of the host's calls](#refusals-of-the-hosts-calls).

On the host, as root, `hostkeeper admin status` reports its enrollment over the daemon's admin
socket (`/run/hostkeeper/admin.sock`, [docs/admin.md](../admin.md)), and a Linux node's console
shows it ([Check the key hash](#check-the-key-hash)).

## The pending request and its match

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment list --limit 50
```

`list` gives the pending requests, oldest first, at most 256 a page. Each has its `request_id`
and `key_sha256` (the same value: the SHA-256 of the request's public key), the host's `claims`
(machine id, DMI serial and product UUID, network addresses, BMC MAC; serial and platform UUID on
a Mac), `image`, `first_seen_unix`, `renewal_of` for a returning host, `duplicate_claim` and
`mac_nonce_presented`, and how it matches the inventory. `--output json` prints the field names
in lower camel case (`keySha256`) and leaves out a field at its default (`false`, an empty
string).

The match is worked out when the request is listed and again when it is approved, from the
records as they are then. A record matches a Linux request when one of its data MACs is among the
claimed addresses, and the request claims no BMC MAC or the record's; a BMC MAC that differs from
the record's means no match. A record that holds only a serial never matches a Linux request.

- **`INVENTORY_MATCH_MATCHED`**: exactly one record matches; `record_host_id` and `record_role`
  are its host id and role, and `record_serial` and `record_bmc_mac` its serial and BMC MAC
  (empty when it holds none). Check the key hash, then approve as that record (below).
- **`INVENTORY_MATCH_UNMATCHED`**: no record matches. Either the server has no record (add one,
  and list again) or its record has the wrong MACs (remove it, add the right one, and list
  again). An approval of an unmatched request must name the host id.
- **`INVENTORY_MATCH_AMBIGUOUS`**: several records match; `candidate_host_ids` lists them all and
  none is chosen. Two records share this server's MACs between them: remove the wrong ones until
  one matches, or approve naming the host id.

Matched is not proof. A request that claims a data MAC of a record, and does not contradict its
BMC MAC, matches it, and the machine wrote those claims itself: anyone who read the label or the
network can write the same. The claims that are hard to copy are the DMI serial, the BMC MAC and
the product UUID, which are not on the data network; compare them with the record's
(`record_serial`, `record_bmc_mac`; the record holds no product UUID). The web app's Enrollment
page puts each beside the record's value, says "not recorded" for a value the record does not
hold and "not claimed" for a claim the request leaves out, and warns when the record holds a BMC
MAC the request does not claim (it matched on a data MAC alone). Approve only the key hash shown
on the machine's own console.

`contested_by` is set on a request when another pending request matches a record it matches: it
lists those requests' ids, and each of them lists this one. A contested request cannot be
approved (`record_contested`), with or without `--replace`, until all but one are rejected. One
of them is likely not the machine on the label, for example a request posted with the recorded
MACs before the machine first booted. Compare each request's `key_sha256` with the key hash on
the machine's console (below), reject the requests that are not it, then approve the one that
is. A request that matches several records (`INVENTORY_MATCH_AMBIGUOUS`) contests the requests
of each. `contested_by` is empty in the `--unenrolled` list: an expired request contests nothing.

`duplicate_claim` is set on a request that another host's identity collides with: another request
has the same machine id, a MAC it claims belongs to a host already enrolled (only universally
administered unicast MACs count), a renewal claims another machine id than the host's, or the
same key came back with another machine id. The coordinator logs `duplicate enrollment claim` at
warn level with the request id, the machine id and which of these it was. A server rebuilt with
a new key, or a cloned disk, looks like this. Approving it needs `--replace true`. A request
that is not one of your servers is not approved: [reject it](#reject), which takes it out of the
queue and refuses its key; otherwise it keeps its place in the queue until it expires (below).

## The Mac path

A Mac's daemon claims its serial and platform UUID (from `ioreg`) and no Linux claim. A record
matches a Mac request when its serial equals the claimed serial, letters compared without case;
MACs are not compared. Record a Mac with `--serial` and no MAC:

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment add-record --host-id mac-03 --role build --serial C02XK1ABJG5J
```

The enrollment request can carry a one-time nonce from MDM (`mac_nonce`, one hour, one use),
which lets the coordinator approve a Mac whose serial is in `--mac-serial-allowlist` without an
operator ([enrollment-coordinator.md](../enrollment-coordinator.md)). The MDM puts the nonce in the
managed preferences of the domain `dev.hostkeeper.enrollment`, which macOS writes to
`/Library/Managed Preferences/dev.hostkeeper.enrollment.plist`: the key `nonce` (the bytes, as
data) and the key `request_id` (the request's `key_sha256`). While a Mac is not enrolled, its daemon
reads that file at every enrollment step, every 5 seconds. When the file's `request_id` is the
daemon's own pending request and its nonce has not been posted by this run of the daemon, the
daemon posts its request once more with `mac_nonce` set, then polls as before; `mac_nonce_presented`
turns true on the request. Each nonce is posted once; a new nonce from the MDM is posted once more.
The daemon reads the file only when root owns it and no one else can write it. A file that is
missing, not root's, writable by others, not a property list, without a 32-byte `nonce` or a
`request_id` of 64 lower-case hex digits, or for another request is ignored and logged nowhere:
the request waits for an operator as any other, and the journal shows only `not enrolled; waiting
for approval`. No line the daemon writes holds a byte of the nonce. To see what the daemon reads,
as root: `plutil -p '/Library/Managed Preferences/dev.hostkeeper.enrollment.plist'` and `ls -l` on
it.

The coordinator's MDM gate (`--mdm-api-url`, with `--mdm-profile-identifier` and
`--mdm-daemon-config`) installs that profile once the Mac's request is pending, bound to it, and
audits each push `mdm_nonce_pushed` by `mdm-gate`. A Mac with two pending requests that claim its
serial and its platform UUID gets no nonce at all: deny the one that is not its daemon's, and the
gate pushes a nonce to the other at its next look.

With `--mdm-pkg-allowlist` and `--mdm-host` the gate also sends the Mac the newest pkg on its
allowlist, before its daemon has posted, and again an hour after the Mac's last answer until a
request from it appears; each send is audited `mdm_install_sent` by `mdm-gate` with its count, and
`hostkeeper ctl mdm list-devices` shows the count and the last send
([enrollment-coordinator.md](../enrollment-coordinator.md)). A Mac whose count grows and that never
posts did not install the pkg: read `/var/log/install.log` on the Mac.

A coordinator without those flags pushes no nonce: every Mac then waits for an operator, with
`mac_nonce_presented` false, and is approved as a Linux request is. So does a Mac the gate does
not see enrolled and supervised, or whose serial is not on `--mac-serial-allowlist`.

With nonces, three things follow for an operator. A nonce approves
only the request it was issued for: a second request presenting it (another key on the same Mac)
stays pending and is audited `mac_auto_approve_declined` with `nonce_wrong_request`; deny it.
A Mac approved by its nonce is assigned the services of its allowlist entry, audited `assign` by
`mdm-nonce`, as an approval assigns the services it names. A Mac wiped and enrolled in MDM again
takes its own host id over from its old key without an operator (`enroll_replace` by
`mdm-nonce`); that does not apply to a denied host id, which stays `denied`, nor to a host id
approved for a request that claimed another serial, which is `host_id_taken`: approve those by
key hash with `--replace true` as below.

## Check the key hash

Claims identify nothing: any machine can claim any MAC. Before approving, check that the request
is the key of the server in front of you.

On a Linux node, the login prompt of its console shows the hash. After each step of its
enrollment, whenever the text changes, the daemon writes the request's state, the coordinator it
posts to and the key hash to `/run/hostkeeper/enrollment.issue`, and asks the gettys to draw
their prompt again (`agetty --reload`); the deb, the rpm and the boot-image overlay link
`/etc/issue.d/hostkeeper.issue` to that file, and agetty shows it above the prompt:

```
hostkeeper enrollment: pending approval; approve only the request whose key_sha256 is the key sha256 below
  coordinator: https://coordinator.example
  key sha256:  4f2a9c0e8b7d6a5f4e3d2c1b0a99887766554433221100ffeeddccbbaa998877
```

The first line says instead `nothing sent: the coordinator's CA is not the pinned ca_sha256`
when `ca_sha256` or `coordinator` is wrong, `error: <why>` when the coordinator cannot be reached
or another step failed (the daemon's log says the same), `denied: <reason>` for a denied request,
and `approved, enrolled as host:<id>` once the host holds its certificate. The local console
(tty1) shows it; a serial console shows it only when a getty runs on it. The text is made of
printable ASCII only: any other character, and a backslash, is written as `?`.

Without a console, on the node, as root:

```
openssl pkey -inform DER -in /etc/updater/pki/pending/key -pubout -outform DER | sha256sum
```

The key is a P-256 key, PKCS#8 DER; the hash is that of its public key's DER
SubjectPublicKeyInfo, and must equal `key_sha256` of the request. Once the host is enrolled, the
same key is `/etc/updater/pki/current/key`. `hostkeeper admin status` gives it too, as
`enrollment.key_sha256` ([docs/admin.md](../admin.md)).

## Approve

An approval names the request, its key hash, the host's role, the services to assign (each one
the coordinator's rollout file defines; none is allowed) and a reason. The role is required even
when the record names one: the record's role is shown, not applied. The certificate is not in the
answer: the host takes it at its next poll, within seconds.

A request that matches one record, approved as that record's host id (no `--host-id`):

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment approve --request-id <request-id> --key-sha256 <key-sha256> \
    --role web --services api --reason "racked in row 4, key checked on the console"
```

A request that matches no record or several, or a renewal, approved under a host id the operator
names:

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment approve --request-id <request-id> --key-sha256 <key-sha256> \
    --host-id web-17 --role web --reason "no record yet, key checked on the console"
```

`--replace true` is needed, and only applies, when the approval takes something from another:

- the host id is held by another key (`host_id_taken` without it): a server rebuilt with a new
  key, or a host moved to new hardware. The host id moves to the new key; the old key's
  certificate is no longer admitted (its heartbeats are refused `forbidden`) and cannot renew;
- the request is `duplicate_claim` (`duplicate_claim` without it);
- the request matches one record and the approval names another host id (`record_mismatch`
  without it);
- the host id was denied (`denied` without it).

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment approve --request-id <request-id> --key-sha256 <key-sha256> \
    --host-id web-17 --role web --services api --replace true \
    --reason "web-17 rebuilt on new disks, key checked on the console"
```

Without `--replace` none of these is changed. For a renewal, `--replace` lifts only
`duplicate_claim`: a renewal is approved under the host id it renews, and no other.

When the approval stands but its services could not be assigned, the coordinator logs `the
services of an approval were not assigned: assign them with Services.Assign`. Assign them:

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    services assign --node-id web-17 --service api
```

## Reject

A rejection names the request, and a reason if you give one (1 to 256 bytes; it may be left
out). It asks nothing of the node: the request leaves the queue, which frees its place under the
[cap](#the-pending-enrollment-cap), and its key is refused from then on. The node's next `Poll`
is answered `DENIED`, and a repost of the same key changes nothing. The rejection is audited
`enroll_reject` with the request id, the claims and the reason; rejecting a request already
rejected succeeds and audits nothing.

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    enrollment reject --request-id <request-id> --reason "not one of ours"
```

A request already approved or issued is `not_pending`: that host is retired, not rejected. A
rejected node gets in again only under a new key, as in [enrolling a node
again](#enrolling-a-node-again).

## The first heartbeat

On the node, the journal shows `enrolled as host:web-17 (generation 1)` once the certificate is
stored. The heartbeat loop then sends its first heartbeat within `--heartbeat-secs` (15 seconds
by default) over mutual TLS, and writes nothing while heartbeats succeed; a failed one is a line
beginning `error: `.

On the coordinator, nothing is logged for a host's first heartbeat; a refused heartbeat is logged
at warn level as `refused a heartbeat: <why>`. `releases status` lists the host, with `live:
true`, under each service assigned to it once its heartbeat names that service, and keeps it
`live` while it heartbeats within 45 seconds:

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    releases status --service api
```

`hosts get --node-id <host id>` answers the host once it has heartbeated, with the inventory record
its enrollment matched (`inventory_record_id`, `data_macs`, `bmc_mac`, `serial`; empty when none
or several match), when it was approved (`enrolled_unix`), and when the coordinator accepted its
first heartbeat since that approval (`first_heartbeat_unix`, by the same clock; absent until
then). The first heartbeat time is kept in the enrollment store with the host, so a coordinator
restart keeps it, and each later approval of the host (a new key with `--replace`, or a renewal an
operator approves) clears it until the host is heard again; the time to first heartbeat of an
install is `first_heartbeat_unix - enrolled_unix`. The Host page of the operator web app shows it
as First heartbeat ("not yet" for an enrolled host not heard since). `hosts list` answers every
host the same way, a page at a time. [Renewal](#renewal) says what they answer of its certificate.

## Renewal

A host certificate lasts 7 days. From half its life the daemon renews it with `Renew`, over
mutual TLS with the certificate it holds, for the same key; no operator is involved. The node
logs `renewed the certificate of host:<id> (generation <n>)` and the coordinator `renewed a host
certificate`. A refused renewal is logged on the node as `renewal refused: <token>`.

Every heartbeat also says when the host's certificate ends, why its last enrollment or renewal
step failed (until a step succeeds), and the host's own clock. `hostkeeper ctl hosts get
--node-id <id>` answers the first two as `cert_not_after_unix` and `last_renew_error`, and
`clock_skew_beats`: how many of the host's heartbeats since the coordinator started said a time
more than `--clock-skew-secs` (120 by default) from the coordinator's clock, either way. The Host
page of the operator web app shows the end and the error, warns while the end is less than 48
hours away (or past) or an error is reported, and shows the count. A daemon whose enroller
cannot be opened (its pki directory locked or unreadable, its coordinator address refused) logs
`error: the enrollment loop cannot start yet: <why>` (once, and again when the reason changes),
reports it as its
`last_renew_error`, and tries again after 5 seconds, then 10, 20 and so on, at most every 5
minutes, for as long as it runs.

A host that was offline until its certificate expired cannot renew it. Its daemon posts a new
request for the key it holds, with `renewal_of` set to its host id, and waits as a new host does
(`not enrolled; waiting for approval`). The request is listed with `renewal_of` and the same
`key_sha256` as before; its inventory match is shown but not used. Approve it with `--host-id`
equal to `renewal_of`, the role again and the services again: they replace the host's. The node
then logs `enrolled as host:<id> (generation <n>)`. A request whose `renewal_of` names a host that
is unknown, denied or certified for another key is refused `renewal_not_allowed`.

## Retire a node

Retire a node that is lost, dead or replaced. A deny row is written for its host id: its
heartbeats and its `Renew` are refused `denied` at once, though its certificate stays valid until
it expires (7 days at most). Every entry of its own in the services' assignments is dropped: an
assignment or exclusion of the host (`services assign --node-id`, `services unassign --node-id`),
its minimum version (`services set-min-version` of the host), and an uninstall it was ordered,
which is cancelled. What its role or the rollout file assigns is not. It is sent no `uninstall`:
what it runs stays as it is. The reason may be left out.

```
hostkeeper ctl --endpoint https://<private-listener> --ca-file <ca.pem> --token-file <token-file> \
    hosts retire --node-id web-17 --reason "board failed, replaced by web-23"
```

The answer's `update_in_flight` says whether the node was updating. If it was, it keeps its
update slot until its heartbeat reports the update's record final; a node that is gone never
does, so free its slot with `hosts exclude --abandon true` for the service it was updating. A node
with no update in flight frees its slot at once. The retirement is audited `host_retire`;
retiring a node already retired succeeds and audits nothing. If a retirement is refused after its
deny row was written (`audit_unavailable` or `store_failed`), the node is refused already; ask
again once the coordinator can write, and the retirement is finished and audited. A host id no host was enrolled as is
`unknown_host` (NOT_FOUND).

A retired host id is taken again only by an approval with `--replace true`.

## Enrolling a node again

A node whose key no longer names a host (its host id was approved for another key, or the
coordinator lost the host), or whose host is marked duplicate, is refused `renewal_not_allowed`
for as long as it keeps that key. To enroll it again under a new key, on the node as root:

1. `systemctl stop hostkeeper.service`.
2. Move `/etc/updater/pki` aside (keep it until the node is enrolled again); do not edit inside
   it.
3. `systemctl start hostkeeper.service`. The daemon makes a new pki directory and key, and posts
   a new request.
4. Check the new key hash and approve the request as above. It is flagged `duplicate_claim`,
   since its machine id is that of the node's earlier request, so the approval needs
   `--replace true`.

## The pending-enrollment cap

The coordinator holds at most `--max-pending-enrollments` requests waiting for approval (256 by
default), `duplicate_claim` ones included. At the cap a new request, and a returning host's
request, is refused `pending_full` and nothing is written: the requests already pending are kept,
and a host whose request is already pending is still answered. The refused daemon logs the
refusal once and posts again every 5 seconds, so it gets in when a place frees. An approval or a
[rejection](#reject) frees a pending request's place, and so does its expiry. A request no
operator approves within `--enrollment-request-ttl` (72 hours by default) of when the coordinator
first received it expires at the next reload tick, and its host is unenrolled: it is audited
`request_expired`, it is no longer approved (`not_pending`), and it is listed with
`enrollment list --unenrolled true` with the reason and when it expired. The host's poll still
answers pending; once its daemon restarts and posts the same key again, the request waits again,
from that post; reject an expired request to keep its key out. Reject the requests nobody will
approve; a queue that still fills needs a larger `--max-pending-enrollments` or a shorter
`--enrollment-request-ttl`, set at the coordinator's next start.

## Refusals of the operator calls

The `Enrollment` service (`list`, `approve`, `reject`, `add-record`, `list-records`,
`remove-record`)
refuses with exactly one of these tokens; `hostkeeper ctl` exits 2 on INVALID_ARGUMENT and
FAILED_PRECONDITION, 3 on UNAUTHENTICATED and PERMISSION_DENIED, and 1 on the others.

| token | code | cause | fix |
|---|---|---|---|
| `unauthorized` | UNAUTHENTICATED | no bearer token, or one that is not an operator token | pass an operator token with `--token-file` |
| `forbidden` | PERMISSION_DENIED | a CI token | use an operator token |
| `unknown_request` | NOT_FOUND | `approve` names a request the coordinator does not hold, or enrollment is off (`--enrollment-dir` not given) | copy `request_id` from `list` again |
| `key_mismatch` | FAILED_PRECONDITION | `--key-sha256` is not the request's key hash | check the hash on the node and pass the listed `key_sha256` |
| `host_id_taken` | ALREADY_EXISTS | another key holds the host id | approve another host id, or `--replace true` when this server is that host now |
| `pending_full` | RESOURCE_EXHAUSTED | the pending queue is full; returned to the host's `Enroll`, never by these calls | see [the cap](#the-pending-enrollment-cap) |
| `duplicate_claim` | FAILED_PRECONDITION | the request is flagged `duplicate_claim` | find which server it is; `--replace true` when it is yours |
| `invalid_host_id` | INVALID_ARGUMENT | the host id breaks its rule (1 to 63 of `a-z0-9-`, not starting or ending with `-`), none is named for a request that matches no record or several or for a renewal, or a renewal is approved under another host id | name a valid host id; for a renewal, `renewal_of` |
| `invalid_role` | INVALID_ARGUMENT | the role is missing or breaks its rule | pass `--role`: 1 to 32 of `a-z0-9-`, starting with a letter, not ending with `-`, with no `--` |
| `invalid_service` | INVALID_ARGUMENT | a service name breaks its rule or names no service the rollout file defines | name only services of the rollout file |
| `invalid_request` | INVALID_ARGUMENT | another field breaks its rule: a request id or key hash that is not 64 hex digits, a missing or bad reason, a page token, or a record's MAC, serial or notes | correct the field; `--reason` is 1 to 256 bytes |
| `not_pending` | FAILED_PRECONDITION | the request was already approved, issued or denied, or it expired; a `reject` of a request approved or issued | list again; an expired request is approved only after its host posts again (retire an enrolled host with `hosts retire`) |
| `denied` | FAILED_PRECONDITION | the host id was retired (`hosts retire`) | `--replace true`, or another host id |
| `store_failed` | INTERNAL | the change could not be written; nothing changed | read the coordinator's log and ask again |
| `record_exists` | ALREADY_EXISTS | `add-record` for a host id that has a record | remove the record first |
| `record_conflict` | ALREADY_EXISTS | `add-record` with a MAC or a serial another record holds | remove or correct the other record |
| `no_record` | NOT_FOUND | `remove-record` of a host id with no record | check the host id with `list-records` |
| `capacity` | RESOURCE_EXHAUSTED | `add-record` past the 10000 records the coordinator keeps | remove records no longer needed |
| `audit_unavailable` | UNAVAILABLE | `add-record` or `remove-record` whose audit line could not be appended (a full disk); nothing changed | free the disk of the audit file, then ask again |
| `record_mismatch` | FAILED_PRECONDITION | `approve` names a host id other than that of the one record the request matches | leave out `--host-id`, or `--replace true` |
| `record_contested` | FAILED_PRECONDITION | `approve` of a request that another pending request contests: they match one record (`contested_by` names the others) | compare each `key_sha256` with the machine's console, and `reject` all but the one that is it; `--replace true` does not set it aside |

## Refusals of the host's calls

The daemon's `Enroll`, `Poll` and `Renew` are refused with one of these tokens. The node logs an
`Enroll` or `Poll` refusal as `the coordinator could not be reached: <call> was answered with the
status <code>: "<token>"` and a `Renew` refusal as `renewal refused: <token>`, and tries again
every 5 seconds.

| token | code | cause | fix |
|---|---|---|---|
| `forbidden` | PERMISSION_DENIED | a `Renew` with no client certificate, or one the coordinator's CA did not issue to a host | none on the node; check the coordinator's CA |
| `pending_full` | RESOURCE_EXHAUSTED | an `Enroll` of a new request while the pending queue is full | approve waiting requests, or raise `--max-pending-enrollments` |
| `key_not_p256` | INVALID_ARGUMENT | the request's key is not an ECDSA P-256 key | none from a hostkeeper daemon, which makes only P-256 keys |
| `bad_csr` | INVALID_ARGUMENT | the CSR is over 4096 bytes, does not parse, is not signed by its own key, or its subject is not one `host:<id>` | none from a hostkeeper daemon; report it |
| `denied` | PERMISSION_DENIED | a `Renew` by a host an operator retired (`hosts retire`) | none on the node: it was retired; it comes back only under a new key, approved with `--replace true` |
| `unknown_request` | NOT_FOUND | a `Poll` for a request the coordinator does not hold | restart the unit: the daemon posts its request again at its start |
| `bad_signature` | UNAUTHENTICATED | a `Poll` whose signature does not verify, or whose nonce is not one the coordinator gave or was used | none: the daemon asks for a new nonce on its next try |
| `renewal_not_allowed` | FAILED_PRECONDITION | an `Enroll` whose `renewal_of` names a host that is unknown, denied or certified for another key; a `Renew` for another key than the certificate's, by a host whose host id went to another key, or by a host marked duplicate (two machines heartbeating under one host id; no path of this build sets that mark) | the node's key no longer names a host: [enroll it again](#enrolling-a-node-again); only such an approval, with `--replace true`, clears a duplicate mark |
