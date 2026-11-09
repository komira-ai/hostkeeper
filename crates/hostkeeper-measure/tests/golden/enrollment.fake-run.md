## A new Linux host enrolled and running its first service

Plan [`enrollment`](../../../crates/hostkeeper-measure/plans/enrollment.toml), run `fake-run`: 1 of 1 repetitions started, 1 complete.

A new host from its first contact with the coordinator to enrolled and running its first service: its enrollment request, the operator's approval (or the inventory's), the certificate issued at the host's next poll, its first heartbeat, and its first heartbeat with a managed service installed, healthy and not updating. Every step is timed by the coordinator's own stamps. Run it when a host is enrolled, with no other enrollment pending.

**Hosts.** One new Linux host, not yet enrolled: host-1 in the records.

**Recorded.** The audit entries `enroll_requested`, `enroll_approve` (or `enrollment.auto_approved`) and `enroll_issued`, each with its time, read with `hkctl audit list`; then the host's `firstHeartbeatUnix` and `firstServingUnix`, read with `hkctl hosts get`. The runner polls every 2 s.

| Measure | What | Unit | n | Missing | Min | Median | Max | Clock |
|---|---|---|---|---|---|---|---|---|
| `operator` | the request to its approval: the operator's time | s | 1 | 0 | 37 | 37 | 37 | coordinator |
| `issue` | the approval to the certificate issued | s | 1 | 0 | 2 | 2 | 2 | coordinator |
| `first-heartbeat` | the certificate issued to the first heartbeat | s | 1 | 0 | 2 | 2 | 2 | coordinator |
| `first-serving` | the first heartbeat to the first service serving | s | 1 | 0 | 30 | 30 | 30 | coordinator |
| `total` | the request to the first service serving | s | 1 | 0 | 71 | 71 | 71 | coordinator |

A coordinator time is the difference of two of the coordinator's stamps, which are whole seconds: an audit entry's time, or a host's last heartbeat as the coordinator listed it at the poll that saw the event, which can be later than the heartbeat that reported the change by up to one poll interval. A runner time is the difference of the runner's clock at the polls that saw the two events, each up to one poll interval after the event.

**What the report states from it.**

- A new host was enrolled and serving its first service 71 s after its enrollment request, of which 37 s was the operator's time before approving.
- From approval, the certificate took 2 s, the first heartbeat 2 s more, and the first service serving 30 s more.

**Repetitions that ended early.** None.

### Values of plan `enrollment`, run `fake-run`

| Measure | Repetition | Host | Value | Unit | Clock |
|---|---|---|---|---|---|
| `operator` | 1 | - | 37 | s | coordinator |
| `issue` | 1 | - | 2 | s | coordinator |
| `first-heartbeat` | 1 | - | 2 | s | coordinator |
| `first-serving` | 1 | - | 30 | s | coordinator |
| `total` | 1 | - | 71 | s | coordinator |
