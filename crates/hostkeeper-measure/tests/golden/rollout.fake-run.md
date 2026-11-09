## A new hostkeeper version rolled out to the farm

Plan [`rollout`](../../../crates/hostkeeper-measure/plans/rollout.toml), run `fake-run`: 3 of 3 repetitions started, 3 complete.

How long a new version of hostkeeper, set as the desired version with `hkctl releases set-desired`, takes to reach every host of the farm under the coordinator's update plan and update slots as they are: the request sets no `maxUpdating`, so each role updates one host at a time (two for the daemon roles), stage by stage. Measured: the time from the set to the first host updated and to the last, and each host's own update, from the poll that first shows it ordered to the poll that first shows the version installed by a succeeded update.

**Hosts.** Every host the service hostkeeper is assigned to: the whole farm, host-1 to host-N in the records.

**Recorded.** The set, stamped by its `set_desired` audit entry; then, for each host, the first poll of `hkctl hosts list` that shows it ordered (it holds an update slot, or its heartbeat reports updating to the version) and the first poll that shows the version installed by an update whose outcome is `OUTCOME_SUCCEEDED`, each stamped with the host's last heartbeat as the coordinator received it. The runner polls every 2 s.

| Measure | What | Unit | n | Missing | Min | Median | Max | Clock |
|---|---|---|---|---|---|---|---|---|
| `first-host` | SetDesired to the first host updated | s | 3 | 0 | 35 | 37 | 39 | coordinator |
| `last-host` | SetDesired to the last host updated | s | 3 | 0 | 115 | 117 | 120 | coordinator |
| `host-update` | a host ordered to that host updated | s | 9 | 0 | 30 | 32 | 35 | coordinator |
| `hosts` | the hosts updated | hosts | 3 | 0 | 3 | 3 | 3 | - |

A coordinator time is the difference of two of the coordinator's stamps, in whole seconds, so within a second either way. A runner time is the difference of the runner's clock at the polls that saw the two events, each up to one poll interval after the event.

**What the report states from it.**

- A new hostkeeper version reached the first host 37 s after it was set (median of 3 repetitions, from 35 s to 39 s).
- It reached the last of 3 hosts 117 s after it was set (from 115 s to 120 s).
- One host's update, from its order to its succeeded update, took 32 s (median of 9 host updates, from 30 s to 35 s).

**Repetitions that ended early.** None.

### Values of plan `rollout`, run `fake-run`

| Measure | Repetition | Host | Value | Unit | Clock |
|---|---|---|---|---|---|
| `first-host` | 1 | - | 35 | s | coordinator |
| `first-host` | 2 | - | 37 | s | coordinator |
| `first-host` | 3 | - | 39 | s | coordinator |
| `last-host` | 1 | - | 120 | s | coordinator |
| `last-host` | 2 | - | 115 | s | coordinator |
| `last-host` | 3 | - | 117 | s | coordinator |
| `host-update` | 1 | host-1 | 30 | s | coordinator |
| `host-update` | 1 | host-2 | 32 | s | coordinator |
| `host-update` | 1 | host-3 | 35 | s | coordinator |
| `host-update` | 2 | host-1 | 32 | s | coordinator |
| `host-update` | 2 | host-2 | 34 | s | coordinator |
| `host-update` | 2 | host-3 | 30 | s | coordinator |
| `host-update` | 3 | host-1 | 34 | s | coordinator |
| `host-update` | 3 | host-2 | 30 | s | coordinator |
| `host-update` | 3 | host-3 | 32 | s | coordinator |
| `hosts` | 1 | - | 3 | hosts | - |
| `hosts` | 2 | - | 3 | hosts | - |
| `hosts` | 3 | - | 3 | hosts | - |
