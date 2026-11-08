# The coordinator on Postgres, with compose

The coordinator as one container and its database, Postgres 16, as a second, on one machine
([coordinator-storage.md](../../design/coordinator-storage.md), sections 9 and 10). The files beside
this page:

| File | What it is |
|---|---|
| `compose.yaml` | The two services, both images pinned by digest, no environment for either, the host's network for both. `postgres` is optional, behind the profile `postgres`. |
| `sql/init.sql`, `sql/password.sql` | The role `hostkeeper` and its database, made once; the password set from its secret file. |
| `sql/after-restore.sql` | The one statement a restore runs before the coordinator starts. |
| `sql/counts.sql` | The rows of each table, printed as `hostkeeper-coordinator export` prints them, for the drill. |
| `hostkeeper-db-backup.service`, `.timer` | A dump every six hours, the newest 28 kept. |

The coordinator's own flags, refusals and the database store's behaviour are in
[coordinator-state.md](../../operations/coordinator-state.md). Every command below runs as root in
the compose directory, `/srv/hostkeeper-coordinator`, which holds:

```
/srv/hostkeeper-coordinator/
  compose.yaml            this directory's compose.yaml
  sql/                    this directory's sql/, mounted read-only into postgres at /etc/hostkeeper-db
  etc/                    the coordinator's configuration, mounted read-only at /etc/hostkeeper-coordinator
  secrets/db-password     the database password, owned by 999, mode 0400
  secrets/db-dsn          the coordinator's connection string, owned by 65532, mode 0400
```

The project's name is the directory's, `hostkeeper-coordinator`, so its volumes are
`hostkeeper-coordinator_hostkeeper-db` (the database) and
`hostkeeper-coordinator_hostkeeper-coordinator` (the coordinator's local volume).

## The images

- **Postgres** is `postgres:16` at the digest in `compose.yaml`, the one CI's tests run against
  (the `postgres` service of `.github/workflows/ci.yml`; a test holds the two equal). Its user
  `postgres` is uid 999.
- **The coordinator** is the image of a release. Download
  `hostkeeper-coordinator-<version>-<platform>.image.tar` and its bundle, verify it as
  [verify-a-release.md](../verify-a-release.md) says, and load it:

  ```sh
  docker load --input hostkeeper-coordinator-<version>-linux-x86_64.image.tar
  tar --extract --to-stdout --file hostkeeper-coordinator-<version>-linux-x86_64.image.tar index.json
  ```

  The second command prints the image's manifest digest (`"digest":"sha256:..."`). Write the
  version and that digest into the coordinator's `image:` line. Docker resolves
  `name:version@sha256:digest` for a loaded image only when its image store keeps the manifest
  digest, as the containerd image store does (`docker info` shows the driver type
  `io.containerd.snapshotter.v1`, the default of Docker Engine 29). `pull_policy: never`: the
  image is never fetched from a registry.

## The secrets

Two files, outside any version control. Compose without swarm mounts a secret as a bind mount
that keeps the file's owner and mode, so each is owned by the user that reads it:

```sh
install --directory --mode=0700 secrets
(umask 077 && openssl rand -hex 32 > secrets/db-password)
(umask 077 && printf 'host=127.0.0.1 port=5432 dbname=hostkeeper user=hostkeeper password=%s sslmode=disable application_name=hostkeeper-coordinator\n' \
  "$(cat secrets/db-password)" > secrets/db-dsn)
chown 999:999 secrets/db-password && chmod 0400 secrets/db-password
chown 65532:65532 secrets/db-dsn && chmod 0400 secrets/db-dsn
```

- `db-password` is read only by `password.sql`, inside the postgres container, as its user
  `postgres` (999).
- `db-dsn` is the coordinator's `--database-dsn-file`: one libpq connection string, owned by the
  coordinator's user (65532), mode 0400 or 0600, at most 4 KiB, as the coordinator requires.
- `printf` is a shell builtin, so the password is on no process's command line.

## The configuration and the local volume

`etc/` holds what the coordinator's flags in `compose.yaml` name under `/etc/hostkeeper-coordinator`:
`tls.pem`, `tls.key`, `client-ca.pem`, `oidc.toml`, `tokens.toml` and `rollout.toml`. The key and
the tokens file are read by the coordinator's user only: owned by 65532, mode 0600. Edit the
command for the farm: the listen addresses (one interface's address each, never a wildcard),
`--server-name`, and any other flag the coordinator runs with today
(`hostkeeper-coordinator serve --help`).

The coordinator gets no compose `healthcheck`: compose runs one inside the container, which has
no shell or client. To probe it from the machine, add `--health-listen` with a loopback address
(for example `127.0.0.1:9100`) to the command, and point the machine's monitoring at
`GET /healthz` ([coordinator-health.md](../coordinator-health.md)).

The local volume, `/var/lib/hostkeeper-coordinator` in the container, holds the CA key
(`ca.key`) and its certificate (`ca.key.cert`), the server certificate file (`server.pem`) and the
marker (`halt-unfinished`). The image runs as 65532, which must own the volume's directory (to
write the server certificate and the marker) and the key (the coordinator refuses a CA key
another user owns or others can read). Make the volume without starting anything, and place
the files in it:

```sh
docker compose create coordinator
volume=$(docker volume inspect --format '{{.Mountpoint}}' hostkeeper-coordinator_hostkeeper-coordinator)
chown 65532:65532 "$volume" && chmod 0700 "$volume"
install --owner=65532 --group=65532 --mode=0600 <the farm's CA key> "$volume/ca.key"
install --owner=65532 --group=65532 --mode=0644 <its certificate> "$volume/ca.key.cert"
```

The server certificate is made at the first start from the CA. A new farm has no CA yet: add
`--ca-init` to the command for the first start, and take it out after; the CA is made once, and
every daemon pins its certificate. Back up the CA key and its certificate once, sealed and
offline ([ca-backup.md](../../operations/ca-backup.md)); the database's dumps do not hold them.

## One-time setup

The database is made by explicit steps, never by the image's `POSTGRES_*` variables:

1. Make the data directory, with the superuser `postgres` reached only through the container's
   own socket (it has no password, so it cannot log in over TCP, where `scram-sha-256` is
   required):

   ```sh
   docker compose run --rm --no-deps --user postgres --entrypoint initdb postgres \
     --pgdata=/var/lib/postgresql/data --username=postgres \
     --auth-local=trust --auth-host=scram-sha-256
   ```

2. Start Postgres: `docker compose --profile postgres up -d postgres`, and wait until
   `docker compose --profile postgres ps postgres` says `healthy`. The image's entrypoint finds
   the data directory made and only starts the server.
3. Make the role and its database:

   ```sh
   docker compose --profile postgres exec -T postgres \
     psql --username=postgres --set=ON_ERROR_STOP=1 --file=/etc/hostkeeper-db/init.sql
   ```

   `init.sql` makes the role `hostkeeper` (it logs in, and is no superuser), runs
   `password.sql`, which reads the password from the mounted secret itself, and makes the
   database `hostkeeper`, owned by the role.
4. Start the coordinator: `docker compose --profile postgres up -d coordinator`. Its first start
   takes the database's advisory lock and runs the migrations. It holds the lock when its lease
   names it:

   ```sh
   docker compose --profile postgres exec -T postgres psql --username=hostkeeper --dbname=hostkeeper \
     --command='SET search_path = hostkeeper' --command='SELECT epoch, holder FROM lease'
   ```

   shows the epoch, which each start raises by one, and the holder, the coordinator's first
   `--server-name`. A start that is refused is in `docker compose logs coordinator`; the
   restart policy starts it again, and a database that is not up yet is refused the same way
   (there is no wait loop in the coordinator).

To move a farm from the file store onto this database, run `hostkeeper-coordinator import`
between steps 3 and 4 ([coordinator-state.md](../../operations/coordinator-state.md), "Moving
between the stores").

## An existing Postgres server

A machine with a Postgres server already leaves the `postgres` service out: start without the
profile (`docker compose up -d coordinator`), and nothing waits for or starts the example
server. The server's administrator makes the role `hostkeeper`, with a password, and the database
`hostkeeper` owned by it, as `init.sql` does (psql runs `password.sql` wherever the password file
is at the path it names). Then `secrets/db-dsn` names that server:

- on the machine's loopback over TCP: `host=127.0.0.1 port=<its port> ...`, as above (the
  coordinator uses the host's network);
- by its Unix socket: mount the server's socket directory into the coordinator, a line
  `- <the socket directory>:/run/postgresql` under its `volumes:`, and write
  `host=/run/postgresql dbname=hostkeeper user=hostkeeper password=<the password> sslmode=disable`.
  The coordinator's user, 65532, must be able to enter that directory and open the socket.

The server's settings must keep commits durable: the coordinator refuses a database with `fsync`,
`synchronous_commit` or `full_page_writes` off. Its backups are that server's own; the timer
below dumps the compose `postgres` service, and its command is the line to change for another
server.

## Changing the password

Write the new password into both secret files in place, then set it and restart the coordinator:

```sh
openssl rand -hex 32 > secrets/db-password
printf 'host=127.0.0.1 port=5432 dbname=hostkeeper user=hostkeeper password=%s sslmode=disable application_name=hostkeeper-coordinator\n' \
  "$(cat secrets/db-password)" > secrets/db-dsn
docker compose --profile postgres exec -T postgres \
  psql --username=postgres --set=ON_ERROR_STOP=1 --file=/etc/hostkeeper-db/password.sql
docker compose --profile postgres restart coordinator
```

`>` writes into the file that is there, which keeps its owner and mode (root writes it whatever
its mode). Do not remove a secret file and make a new one: a running container keeps seeing the
file it was started with, so `password.sql` would read the old password.

## Backups

`hostkeeper-db-backup.service`, started by `hostkeeper-db-backup.timer` every six hours, runs
`xtask db-backup` (the installed xtask binary, `/usr/local/libexec/hostkeeper/xtask`), which runs

```sh
docker compose --project-directory /srv/hostkeeper-coordinator --profile postgres \
  exec -T postgres pg_dump --format=custom --username=hostkeeper --dbname=hostkeeper
```

over the container's own socket, with its output into a new file
`/var/lib/hostkeeper-db-backup/hostkeeper-db-<n>.dump` (mode 0600, the directory 0700), numbered
one past the highest there. A dump that fails or writes nothing is not kept, and removes no older
one; a good one removes the oldest beyond the newest 28. The coordinator keeps running: a dump is
one consistent snapshot. Install the units as their header says, and copy the directory off the
machine.

The dumps do not hold the CA key and its certificate (a sealed offline copy), the files of the
local volume or `etc/`: copy those once, as the move procedure of
[coordinator-state.md](../../operations/coordinator-state.md) says.

## Restore

Into an empty database only, in one transaction, so a restore that fails leaves nothing behind:

```sh
docker compose --profile postgres stop coordinator
docker compose --profile postgres exec -T postgres dropdb --username=postgres hostkeeper
docker compose --profile postgres exec -T postgres createdb --username=postgres --owner=hostkeeper hostkeeper
docker compose --profile postgres exec -T postgres \
  pg_restore --single-transaction --exit-on-error --username=hostkeeper --dbname=hostkeeper \
  < /var/lib/hostkeeper-db-backup/hostkeeper-db-<n>.dump
docker compose --profile postgres exec -T postgres psql --username=hostkeeper --dbname=hostkeeper \
  --set=ON_ERROR_STOP=1 --file=/etc/hostkeeper-db/after-restore.sql
docker compose --profile postgres up -d coordinator
```

`after-restore.sql` moves the enrollment serial counter on by 1,048,576, so a certificate issued
after the restore never takes a serial that one issued after the dump already had. It is a step of
the restore: the hazard is closed only when it runs, before the start.

A restore loses what was changed after the dump: enrollments, operator changes, desired versions
and spent CI token ids. A host that enrolled after the dump enrolls again; see "Hosts enrolled
after the last backup" in [coordinator-state.md](../../operations/coordinator-state.md). A start
that then refuses for its marker (changes it held in memory that the restored database does not
have) names them; they are after the dump, so remove the marker knowingly and start again.

## The restore drill

Monthly, and after any release that adds a migration. It restores the newest dump into a scratch
database on the same server, reads every row back through the readers a start uses, and compares
the counts:

1. Take a dump now, `systemctl start hostkeeper-db-backup.service`, and at once read the live
   counts:

   ```sh
   docker compose --profile postgres exec -T postgres psql --username=hostkeeper --dbname=hostkeeper \
     --quiet --tuples-only --no-align --set=ON_ERROR_STOP=1 --file=/etc/hostkeeper-db/counts.sql > live.counts
   ```

2. Restore that dump into `hostkeeper_drill`:

   ```sh
   docker compose --profile postgres exec -T postgres createdb --username=postgres --owner=hostkeeper hostkeeper_drill
   docker compose --profile postgres exec -T postgres \
     pg_restore --single-transaction --exit-on-error --username=hostkeeper --dbname=hostkeeper_drill \
     < /var/lib/hostkeeper-db-backup/hostkeeper-db-<n>.dump
   ```

3. Write a second connection-string file, `secrets/db-dsn-drill`, as `db-dsn` with
   `dbname=hostkeeper_drill` (owned by 65532, mode 0400), make an empty scratch directory
   `drill/` owned by 65532 with mode 0700, and export the scratch database into it:

   ```sh
   docker compose run --rm --no-deps -T \
     --volume /srv/hostkeeper-coordinator/secrets/db-dsn-drill:/run/drill-dsn:ro \
     --volume /srv/hostkeeper-coordinator/drill:/drill \
     coordinator export --database-dsn-file /run/drill-dsn --state-file /drill/desired.json \
     --ops-file /drill/desired.json.ops --enrollment-dir /drill/enrollment --into-empty > drill.out
   ```

   (add `--legacy-service <name>` when the coordinator runs with one). `export` loads every row
   as a start does, writes the file store's files, and prints the rows of each table.
4. Compare: the lines of `drill.out` after `the rows of each table:`, up to the audit line, must
   equal `live.counts`, line for line. A difference is a change made between the dump and the
   count (the audit table says which); take both again if one is there.
5. Drop the scratch database and the scratch files:

   ```sh
   docker compose --profile postgres exec -T postgres dropdb --username=postgres hostkeeper_drill
   ```

   and remove `drill/`, `secrets/db-dsn-drill`, `live.counts` and `drill.out`.

A drill that fails (the restore, the export, or counts that differ with no change to explain them)
means the dumps are not trusted until one passes.
