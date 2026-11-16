# Probe the coordinator's health

The coordinator's container image is distroless: no shell and no client, so nothing inside the
container can run a health check, and the image has no `HEALTHCHECK`. Health is a route instead:
`serve --health-listen <address:port>` binds one more listener, and a probe on the same machine
asks it `GET /healthz` (is the process alive: what the orchestrator checks) or `GET /readyz` (does it
take traffic: what the edge checks). The design is [two-programs.md](../design/two-programs.md),
section 2.7; the contract of the two is the service update contract (one instance, two during a
start-first update, Postgres decides).

## The flag

```sh
hostkeeper-coordinator serve ... --health-listen 127.0.0.1:9100
```

- **Loopback only.** The address must be a loopback one: `127.0.0.0/8` or `::1` (an IPv4 loopback
  address written as IPv6, `[::ffff:127.0.0.1]`, is taken too). Any other address, a wildcard
  such as `0.0.0.0` or `[::]` among them, stops the start with
  `--health-listen: <address> is not a loopback address; ...`. Nothing off the machine reaches the
  listener.
- **Plain HTTP, no token.** The listener speaks HTTP/1.1 with no TLS and reads no token: a probe
  needs neither a secret nor the farm's CA. Each connection answers one request and is closed.
- **Without the flag** there is no health listener.
- **The start line** names it last, as `health=<address:port>`, after `heartbeat=`, `control=`
  and, with `--lan-listen`, `lan=`.

The coordinator runs on the host's network (as `--private-listen` needs), so the loopback address
inside the container is the machine's own, and a probe on the machine reaches it.

## What it answers

| Request | Status | Body |
|---|---|---|
| `GET /healthz` or `GET /readyz` (a query is ignored), serving | `200` | `ok` |
| `GET /healthz` or `GET /readyz`, a listener not yet bound | `503` | `starting` |
| `GET /readyz`, the database's link lost | `503` | `store_unreachable` |
| `GET /readyz`, from SIGTERM or SIGINT until the process exits, whatever else holds | `503` | `draining` |
| `GET /healthz`, the database's link lost, or from SIGTERM or SIGINT until the process exits | `200` | `ok` |
| `HEAD` of either | as `GET` | none |
| any other path, or any other method (`POST` among them) | `404` | `not_found` |

`/healthz` says the process is alive: it is `ok` from the moment every listener is bound, whatever
the database does and while the coordinator drains, so that an orchestrator that checks it never
kills a coordinator for a database that does not answer or for a stop in progress. `/readyz` says
the coordinator takes traffic, and is the one an edge checks.

`200 ok` means every listener the flags name is bound and serving and the store takes writes: with
the Postgres store (`--database-dsn-file`), this coordinator holds the database's lock, shared
with any other coordinator of the database
([coordinator-storage.md](../design/coordinator-storage.md), section 5). The file store's link is
never lost, so a file-store coordinator is `ok` once it serves. The body is the word and nothing
else, as `text/plain` with `cache-control: no-store`: no version, no host, no count. An answer
reads memory only and writes no audit line.

- `starting` is answered from the moment the health listener is bound, which is before the other
  listeners: the start reads its files and opens its store first, and only then binds.
- `store_unreachable` stands while the session with the database is lost. The coordinator keeps
  answering heartbeats meanwhile and orders nothing; the `store_unreachable` alert of
  `Alerts.List` says the same.
- `draining` is answered by `/readyz` from the moment the coordinator takes SIGTERM or SIGINT until
  it exits, ahead of every other token: every answer of `/readyz` made after the signal is
  `draining`, never `ok`, a probe that connected before the signal among them. It never turns back
  into `ok`. The coordinator keeps serving its listeners for `--drain-delay-secs` after the signal
  (default 15; 0 stops serving and exits at once), so that an edge that checks `/readyz` sees the
  `503` and an instance that replaces this one is already taking connections; then it exits, and
  from then on a probe gets no answer. A start-first update needs `--drain-delay-secs` at least the
  edge's DNS refresh plus its healthy and unhealthy thresholds times its check interval (9 seconds
  at the edge's values, so 15 has 6 of margin), and the service's stop grace period above it.

The listener has the limits of the others: at most 1024 connections at once, ten seconds for a
request head and for each write of an answer the probe does not read.

## Probing it

Any HTTP probe on the machine that treats `200` as healthy and anything else, or no answer, as
not, for example:

```sh
curl --fail --silent --show-error --max-time 5 http://127.0.0.1:9100/healthz
```

Swarm's own check is `/healthz` (a coordinator whose database does not answer is alive: it is told
to take no traffic by `/readyz`, not killed); the edge's active check is `/readyz`.

In the compose deployment, compose runs a `healthcheck` inside the container, where there is no
client, so the compose file has none for the coordinator: the probe is the machine's own
monitoring or an orchestrator's HTTP probe. Compose's restart policy still restarts a coordinator
that exits.
