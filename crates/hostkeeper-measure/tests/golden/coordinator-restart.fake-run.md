## The coordinator restarted on its Postgres store

Plan [`coordinator-restart`](../../../crates/hostkeeper-measure/plans/coordinator-restart.toml), run `fake-run`: 3 of 3 repetitions started, 3 complete.

What a restart of the coordinator on its Postgres store costs the fleet: the operator's requests that fail while it restarts, how long it does not answer, the longest gap between two heartbeats of a host it records, and the time until every host is back in `hkctl hosts list`, live with a heartbeat newer than its last before the restart. A daemon that cannot reach the coordinator tries again at its next heartbeat and resends the verdicts the coordinator has not acknowledged, so a heartbeat is delayed rather than lost; the gap shows by how much.

**Hosts.** Every host the coordinator lists as live when the repetition starts: host-1 to host-N in the records.

**Recorded.** Every poll of `hkctl hosts list` that fails, and the first that answers after failures; the moment the runner first sees the coordinator gone (a failed poll, or a host no longer listed live); each host's heartbeat stamp each time it changes; and each host's return. The runner polls every second, from just before the restart until every host is back.

| Measure | What | Unit | n | Missing | Min | Median | Max | Clock |
|---|---|---|---|---|---|---|---|---|
| `unavailable` | the first failed request to the first answer after it | s | 3 | 0 | 4 | 8 | 17 | runner |
| `failed-requests` | the runner's polls that failed | requests | 3 | 0 | 4 | 8 | 17 | - |
| `all-back` | the coordinator seen gone to the last host back | s | 3 | 0 | 18 | 22 | 31 | runner |
| `host-back` | the coordinator seen gone to the host back | s | 9 | 0 | 8 | 18 | 31 | runner |
| `heartbeat-gap` | the longest time between two heartbeats of a host | s | 9 | 0 | 15 | 30 | 45 | coordinator |

A coordinator time is the difference of two of the coordinator's stamps, which are whole seconds: an audit entry's time, or a host's last heartbeat as the coordinator listed it at the poll that saw the event, which can be later than the heartbeat that reported the change by up to one poll interval. A runner time is the difference of the runner's clock at the polls that saw the two events, each up to one poll interval after the event.

**What the report states from it.**

- While the coordinator restarted, the runner's requests failed for 8 s (median of 3 repetitions, from 4 s to 17 s), at most 17 polls in one repetition.
- Every host was back in hkctl hosts list 22 s after the runner saw the coordinator gone (from 18 s to 31 s).
- The longest gap between two heartbeats of one host was 45 s (median 30 s over 9 hosts and repetitions).

**Repetitions that ended early.** None.

### Values of plan `coordinator-restart`, run `fake-run`

| Measure | Repetition | Host | Value | Unit | Clock |
|---|---|---|---|---|---|
| `unavailable` | 1 | - | 8 | s | runner |
| `unavailable` | 2 | - | 4 | s | runner |
| `unavailable` | 3 | - | 17 | s | runner |
| `failed-requests` | 1 | - | 8 | requests | - |
| `failed-requests` | 2 | - | 4 | requests | - |
| `failed-requests` | 3 | - | 17 | requests | - |
| `all-back` | 1 | - | 22 | s | runner |
| `all-back` | 2 | - | 18 | s | runner |
| `all-back` | 3 | - | 31 | s | runner |
| `host-back` | 1 | host-1 | 12 | s | runner |
| `host-back` | 1 | host-2 | 17 | s | runner |
| `host-back` | 1 | host-3 | 22 | s | runner |
| `host-back` | 2 | host-2 | 8 | s | runner |
| `host-back` | 2 | host-3 | 13 | s | runner |
| `host-back` | 2 | host-1 | 18 | s | runner |
| `host-back` | 3 | host-3 | 21 | s | runner |
| `host-back` | 3 | host-1 | 26 | s | runner |
| `host-back` | 3 | host-2 | 31 | s | runner |
| `heartbeat-gap` | 1 | host-1 | 15 | s | coordinator |
| `heartbeat-gap` | 1 | host-2 | 30 | s | coordinator |
| `heartbeat-gap` | 1 | host-3 | 30 | s | coordinator |
| `heartbeat-gap` | 2 | host-1 | 30 | s | coordinator |
| `heartbeat-gap` | 2 | host-2 | 15 | s | coordinator |
| `heartbeat-gap` | 2 | host-3 | 15 | s | coordinator |
| `heartbeat-gap` | 3 | host-1 | 30 | s | coordinator |
| `heartbeat-gap` | 3 | host-2 | 45 | s | coordinator |
| `heartbeat-gap` | 3 | host-3 | 30 | s | coordinator |
