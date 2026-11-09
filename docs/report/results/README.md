# Result files of the technical report

Each measured cell of [the report](https://github.com/komira-ai/hostkeeper/blob/main/docs/report/hostkeeper.html)
names a file of this directory in its `data-result` attribute, and every number the cell prints is
the value of a row of that file that names the cell. The check is `cargo test -p xtask --test report`.

A result file is tab-separated text with the columns `kind`, `id`, `value`, `unit`, `how` and
`cells`. Lines starting with `#` are comments. `value` is a number. `how` says how the row was
obtained (the run and the command, or the arithmetic of a derived row). `cells` lists, separated by
commas, every cell id that prints the value; a row no cell prints leaves it out.

`farm-canary-node-5.tsv` holds the first farm canary: one run of each step on one Linux node of
the lab, node-5. The report's measurements that run did not take are open cells. The check
`canary_tests.rs` (in `crates/xtask/tests/report/`) holds that each row of that file is printed by
exactly the cells it names, and that a cell or a row changed alone fails the check.

`farm-rollout-linux.tsv` holds the rollout to the other five Linux nodes of the lab after that
canary: one `raw` row per node and measurement, which no cell prints, and `derived` rows (the least
and the greatest time, to 0.1 s, and the number of nodes) that the report's two rollout cells print.
The check `rollout_tests.rs` holds that each row is printed by exactly the cells it names, that
each derived row is what the raw rows give, and that a derived row they do not give fails.

## Measurement plans

The measurements still to take on the farm are plans, one file each in
[`crates/hostkeeper-measure/plans`](../../../crates/hostkeeper-measure/plans). A plan says what is
measured, on which hosts, how many repetitions, what is recorded, the steps of a repetition, the
measures computed from the records and what the report states from them; `hostkeeper-measure
check --plan <file>` checks one, and the crate's documentation
([`src/lib.rs`](../../../crates/hostkeeper-measure/src/lib.rs)) describes the commands.

| Plan | What it measures | Hosts | Repetitions |
|---|---|---|---|
| `rollout` | a new hostkeeper version set with `releases set-desired`, to the first host updated, to the last, and each host's own update, with the update plan and slots as they are | every host the service hostkeeper is assigned to | 3 |
| `update-rollback` | on one host, `releases set-desired` of a new hello version to `Succeeded`, and `releases force` back to the previous version (kept on the host, so nothing is fetched) to `Succeeded` | the one host hello is assigned to | 5 |
| `drift` | a hand-made change to what hostkeeper placed for hello, to the coordinator showing it (today: hello unhealthy, since `ServiceBeat.drift` is not built), and its repair by the next order | the one host hello is assigned to | 3 |
| `coordinator-restart` | a restart of the coordinator on its Postgres store: the requests that fail, how long it does not answer, the longest gap between two heartbeats of a host, and the time until every host is back in `hosts list` | every host listed live | 3 |
| `enrollment` | a new Linux host from its enrollment request to its first service serving, by the coordinator's stamps | one new host | 1 |
| `enrollment-mac` | a Mac erased and enrolled through the MDM gate, from its MDM enrollment to its first service serving, by the coordinator's stamps | one Mac | 1 |

No plan has been run yet. A run of a plan becomes a result in three steps:

1. `hostkeeper-measure run --plan crates/hostkeeper-measure/plans/<plan>.toml --run-id <run>
   --param ... --dry-run` prints every step of every repetition with its parameters, and calls
   nothing and writes nothing; it reads the parameters' files to check them.
2. The same with `--run-dir <dir> --hkctl <hkctl> --endpoint <url> --ca-file <ca> --token-file
   <token>` in place of `--dry-run` runs it with `hkctl` and the operator's token, writing into the
   run directory, outside the repository.
3. `hostkeeper-measure analyze --plan crates/hostkeeper-measure/plans/<plan>.toml --run-dir <dir>
   --out-dir docs/report/results` writes `<plan>-<run>.md`, the run's section of the results page,
   and `<plan>-<run>.tsv`, its result file.

The run's `run.json` and `records.jsonl` are committed under `runs/<plan>-<run>/` beside the two
files, so the analysis can be run again from what is committed. The run directory's `hosts.tsv`
(each node id and the name the records give it) and `requests/` (the requests sent) hold the lab's
names and are not committed. A report cell then names `<plan>-<run>.tsv`, and the rows it prints
name the cell; each row's `how` names the plan and the run, and this page links each run's section
below.

### The records

`records.jsonl` holds one JSON object per line, one per event, in the order the runner saw them:

| Field | Meaning |
|---|---|
| `rep` | the repetition, from 1 |
| `step` | the step's id, or `repetition` for its start and end |
| `event` | what happened (below) |
| `host` | the host, as `host-<n>` in the order the run first saw it; no node id or hostname |
| `runner_ms` | the runner's clock when it saw the event, in unix milliseconds |
| `coordinator_unix` | the coordinator's own stamp of the event, in unix seconds, where it has one: an audit entry's `atUnix`, a host's `lastHeartbeatUnix`, `firstHeartbeatUnix` or `firstServingUnix`, a Mac's `enrolledSinceUnix` |
| `detail` | strings: a failed call's `code` and `refused` reason, an audit entry's `action` or `audit_seq`, a repetition's `hosts`, `complete` and `why` |

| Event | Step | Recorded when |
|---|---|---|
| `started`, `ended` | `repetition` | a repetition starts with its hosts picked, and ends, complete or not |
| `called` | a call | `hkctl` answered; stamped by the audit entry the call wrote when the plan names one |
| `call-failed` | a call or a watch | `hkctl` exited with another code than 0 |
| `done` | an operator step | the operator pressed Enter |
| `ordered`, `updated` | a watch of an update | a host first holds an update slot or reports updating to the version; first lists the version installed by a succeeded update |
| `health` | a watch of health | a host first reports the service with the health the plan names |
| `live` | a watch of liveness | a host is first listed live |
| `restart-seen`, `answered`, `heartbeat`, `back` | a watch of a restart | the coordinator first seen gone; its first answer after failures; a host's heartbeat stamp changed; a host listed live again with a newer heartbeat |
| `seen` | a watch of the audit | an entry of the actions the plan names |
| `first-heartbeat`, `first-serving` | a watch of a new host | its first heartbeat and its first service serving, as the coordinator records them |
| `enrolled` | a watch of the MDM gate | a Mac's MDM enrollment, new or renewed |
| `timed-out` | a watch | the condition did not hold within the plan's timeout |

A time the analysis computes is the difference of two stamps of one clock: the coordinator's when
both events have its stamp, the runner's otherwise (each stamp up to one poll interval late). The
coordinator's stamps are whole seconds: an audit entry's time is when the change was made, and a
host's heartbeat stamp is the last heartbeat the coordinator had when the runner polled, up to one
poll interval later than the heartbeat that reported the change. A repetition that did not record
what a measure needs adds to its `missing` count; a time to the last of all hosts (a `last` span) is
taken only from a repetition that ended complete.

### Runs

None yet.
