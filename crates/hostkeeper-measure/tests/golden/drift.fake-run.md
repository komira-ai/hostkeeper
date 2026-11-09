## A hand-made change on a host, detected and repaired

Plan [`drift`](../../../crates/hostkeeper-measure/plans/drift.toml), run `fake-run`: 3 of 3 repetitions started, 3 complete.

After a hand-made change to what hostkeeper placed for hello on one host, the time until the coordinator shows it, and the time to repair it with the next order. The daemon does not report drift yet (`ServiceBeat.drift` is designed, not built): the coordinator sees a change only when the observer finds hello's running executable is not the one sealed for its version, which it reports as unhealthy. A placed file or unit changed by hand is put back only when the daemon starts or switches versions, so such a change is recorded as not detected within the watch's timeout. The repair is the next order: an update to a new version, which links every placed path again. When `ServiceBeat.drift` is built, the detection watch reads it instead.

**Hosts.** The one host hello is assigned to (label one host demo=yes and no other): host-1 in the records.

**Recorded.** The operator's change, stamped by the runner when the operator confirms it; the first poll of `hkctl hosts list` that shows hello unhealthy on the host, or the timeout; the repair's set, stamped by its `set_desired` audit entry; and the first poll that shows the new version installed by a succeeded update with hello healthy, stamped with the host's last heartbeat.

| Measure | What | Unit | n | Missing | Min | Median | Max | Clock |
|---|---|---|---|---|---|---|---|---|
| `detection` | the operator's change to the coordinator showing hello unhealthy | s | 2 | 1 | 8 | 8 | 8 | runner |
| `undetected` | changes the watch did not see within its timeout | changes | 3 | 0 | 0 | 0 | 1 | - |
| `repair` | SetDesired of the repair to the host's succeeded, healthy update | s | 3 | 0 | 10 | 12 | 14 | coordinator |

A coordinator time is the difference of two of the coordinator's stamps, which are whole seconds: an audit entry's time, or a host's last heartbeat as the coordinator listed it at the poll that saw the event, which can be later than the heartbeat that reported the change by up to one poll interval. A runner time is the difference of the runner's clock at the polls that saw the two events, each up to one poll interval after the event.

**What the report states from it.**

- The coordinator showed 2 of the hand-made changes, 8 s after each was made (median, from 8 s to 8 s); 1 it did not show within the watch's timeout.
- The next order repaired it 12 s after SetDesired (from 10 s to 14 s).

**Repetitions that ended early.** None.

### Values of plan `drift`, run `fake-run`

| Measure | Repetition | Host | Value | Unit | Clock |
|---|---|---|---|---|---|
| `detection` | 1 | - | 8 | s | runner |
| `detection` | 3 | - | 8 | s | runner |
| `undetected` | 1 | - | 0 | changes | - |
| `undetected` | 2 | - | 1 | changes | - |
| `undetected` | 3 | - | 0 | changes | - |
| `repair` | 1 | - | 10 | s | coordinator |
| `repair` | 2 | - | 12 | s | coordinator |
| `repair` | 3 | - | 14 | s | coordinator |
