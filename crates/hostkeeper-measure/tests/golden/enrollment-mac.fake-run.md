## A Mac enrolled through the MDM gate and running its first service

Plan [`enrollment-mac`](../../../crates/hostkeeper-measure/plans/enrollment-mac.toml), run `fake-run`: 1 of 1 repetitions started, 1 complete.

A Mac of the farm, erased and enrolled anew through Automated Device Enrollment, from its MDM enrollment to running its first service, through the MDM gate's steps: the hostkeeper package sent, the daemon's enrollment request, the one-time nonce pushed, the approval by that nonce, the certificate issued, the first heartbeat and the first service serving. Every step but the erase is timed by the coordinator's own stamps. Run it when a Mac is erased for the purpose, with no other enrollment pending.

**Hosts.** One Mac of the farm, erased and enrolled anew: host-1 in the records.

**Recorded.** The operator's erase, stamped by the runner; the Mac's `enrolledSinceUnix` as `hkctl mdm list-devices` lists it; the audit entries `mdm_install_sent`, `enroll_requested`, `mdm_nonce_pushed`, `enroll_approve` by `mdm-nonce` and `enroll_issued`, each with its time; and the host's `firstHeartbeatUnix` and `firstServingUnix`. The runner polls every 5 s for the MDM enrollment and every 2 s for the rest.

| Measure | What | Unit | n | Missing | Min | Median | Max | Clock |
|---|---|---|---|---|---|---|---|---|
| `install` | the MDM enrollment to the package sent | s | 1 | 0 | 30 | 30 | 30 | coordinator |
| `nonce-approval` | the daemon's request to its approval by the nonce | s | 1 | 0 | 1 | 1 | 1 | coordinator |
| `issue` | the approval to the certificate issued | s | 1 | 0 | 4 | 4 | 4 | coordinator |
| `first-heartbeat` | the certificate issued to the first heartbeat | s | 1 | 0 | 6 | 6 | 6 | coordinator |
| `first-serving` | the first heartbeat to the first service serving | s | 1 | 0 | 25 | 25 | 25 | coordinator |
| `total` | the MDM enrollment to the first service serving | s | 1 | 0 | 141 | 141 | 141 | coordinator |
| `erase-to-enrolled` | the operator's erase to the MDM enrollment, on the runner's clock | s | 1 | 0 | 300 | 300 | 300 | runner |

A coordinator time is the difference of two of the coordinator's stamps, which are whole seconds: an audit entry's time, or a host's last heartbeat as the coordinator listed it at the poll that saw the event, which can be later than the heartbeat that reported the change by up to one poll interval. A runner time is the difference of the runner's clock at the polls that saw the two events, each up to one poll interval after the event.

**What the report states from it.**

- An erased Mac was enrolled and serving its first service 141 s after its MDM enrollment.
- The MDM gate sent the package 30 s after the MDM enrollment and approved the daemon's request by its nonce 1 s after the request; the certificate took 4 s more, the first heartbeat 6 s and the first service serving 25 s.

**Repetitions that ended early.** None.

### Values of plan `enrollment-mac`, run `fake-run`

| Measure | Repetition | Host | Value | Unit | Clock |
|---|---|---|---|---|---|
| `install` | 1 | - | 30 | s | coordinator |
| `nonce-approval` | 1 | - | 1 | s | coordinator |
| `issue` | 1 | - | 4 | s | coordinator |
| `first-heartbeat` | 1 | - | 6 | s | coordinator |
| `first-serving` | 1 | - | 25 | s | coordinator |
| `total` | 1 | - | 141 | s | coordinator |
| `erase-to-enrolled` | 1 | - | 300 | s | runner |
