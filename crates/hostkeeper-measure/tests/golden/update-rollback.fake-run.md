## An update and a rollback on one host

Plan [`update-rollback`](../../../crates/hostkeeper-measure/plans/update-rollback.toml), run `fake-run`: 5 of 5 repetitions started, 5 complete.

On one host, the time from the order to `Succeeded` for an update of the example service hello to a new version (fetched from the release channel), and for a rollback to the version it ran before, which the host keeps as `previous`, so the rollback fetches nothing. The update is a `hkctl releases set-desired`; the rollback is a `hkctl releases force` of the previous version to that host alone. A force marks the version it leaves bad, so each repetition updates to a new version and rolls back to the same base version.

**Hosts.** The one host hello is assigned to (label one host demo=yes and no other): host-1 in the records.

**Recorded.** The set and the force, each stamped by its audit entry (`set_desired`, `force`); for the host, the first poll of `hkctl hosts list` that shows it ordered and the first that shows the version installed by a succeeded update with hello healthy, each stamped with the host's last heartbeat as the coordinator received it. The runner polls every second.

| Measure | What | Unit | n | Missing | Min | Median | Max | Clock |
|---|---|---|---|---|---|---|---|---|
| `update` | SetDesired to the host's succeeded update | s | 5 | 0 | 12 | 14 | 16 | coordinator |
| `update-order` | the host ordered to its succeeded update | s | 5 | 0 | 9 | 11 | 13 | coordinator |
| `rollback` | the force to the host's succeeded rollback | s | 5 | 0 | 9 | 9 | 11 | coordinator |

A coordinator time is the difference of two of the coordinator's stamps, which are whole seconds: an audit entry's time, or a host's last heartbeat as the coordinator listed it at the poll that saw the event, which can be later than the heartbeat that reported the change by up to one poll interval. A runner time is the difference of the runner's clock at the polls that saw the two events, each up to one poll interval after the event.

**What the report states from it.**

- An update of hello on one host took 14 s from SetDesired to its succeeded update (median of 5 repetitions, from 12 s to 16 s), of which the host's own update, from its order, took 11 s.
- A rollback to the previous version, which fetches nothing, took 9 s from the force to its succeeded update (from 9 s to 11 s).

**Repetitions that ended early.** None.

### Values of plan `update-rollback`, run `fake-run`

| Measure | Repetition | Host | Value | Unit | Clock |
|---|---|---|---|---|---|
| `update` | 1 | host-1 | 12 | s | coordinator |
| `update` | 2 | host-1 | 16 | s | coordinator |
| `update` | 3 | host-1 | 14 | s | coordinator |
| `update` | 4 | host-1 | 12 | s | coordinator |
| `update` | 5 | host-1 | 16 | s | coordinator |
| `update-order` | 1 | host-1 | 9 | s | coordinator |
| `update-order` | 2 | host-1 | 13 | s | coordinator |
| `update-order` | 3 | host-1 | 11 | s | coordinator |
| `update-order` | 4 | host-1 | 9 | s | coordinator |
| `update-order` | 5 | host-1 | 13 | s | coordinator |
| `rollback` | 1 | host-1 | 9 | s | coordinator |
| `rollback` | 2 | host-1 | 11 | s | coordinator |
| `rollback` | 3 | host-1 | 9 | s | coordinator |
| `rollback` | 4 | host-1 | 11 | s | coordinator |
| `rollback` | 5 | host-1 | 9 | s | coordinator |
