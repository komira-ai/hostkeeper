-- older-releases: refuse
-- The coordinator's state (docs/design/coordinator-storage.md, section 3). Run with search_path
-- set to the schema hostkeeper; nothing here names a schema. A shipped migration is never edited:
-- its SHA-256 is pinned by a test, and a change is a migration of its own.

-- The desired state and the registry of every version CI set (section 3.2).
CREATE TABLE desired (
  service        text PRIMARY KEY,
  version        text NOT NULL,
  max_updating   jsonb NOT NULL,
  min_up         jsonb NOT NULL,
  release_digest text,
  release_index  jsonb,
  CHECK (release_digest IS NULL OR release_index IS NULL));

CREATE TABLE desired_digests (
  service text,
  version text,
  release jsonb NOT NULL,
  PRIMARY KEY (service, version));

-- The operations store (section 3.3).
CREATE TABLE services (
  service              text PRIMARY KEY,
  last_good            text,
  halt_generation      bigint NOT NULL DEFAULT 0,
  force_generation     bigint NOT NULL DEFAULT 0,
  uninstall_generation bigint NOT NULL DEFAULT 0,
  force                jsonb);

CREATE TABLE bad_versions (
  service  text,
  version  text,
  by_force boolean NOT NULL,
  position bigint NOT NULL,
  PRIMARY KEY (service, version));

CREATE TABLE failure_acks (
  service  text,
  node_id  text,
  report   jsonb NOT NULL,
  position bigint NOT NULL,
  PRIMARY KEY (service, node_id));

CREATE TABLE host_labels (
  node_id text,
  key     text,
  value   text NOT NULL,
  PRIMARY KEY (node_id, key));

CREATE TABLE exclusions (
  service  text,
  node_id  text,
  reason   text NOT NULL,
  position bigint NOT NULL,
  PRIMARY KEY (service, node_id));

CREATE TABLE holds (
  service  text,
  scope    text CHECK (scope IN ('host', 'role', 'fleet')),
  target   text NOT NULL,
  reason   text NOT NULL,
  by_name  text NOT NULL,
  at_unix  bigint NOT NULL,
  position bigint NOT NULL,
  PRIMARY KEY (service, scope, target));

CREATE TABLE assignments (
  service text,
  scope   text CHECK (scope IN ('host', 'role')),
  target  text,
  entry   text NOT NULL CHECK (entry IN ('include', 'exclude')),
  PRIMARY KEY (service, scope, target));

CREATE TABLE min_versions (
  service text,
  scope   text CHECK (scope IN ('host', 'role')),
  target  text,
  version text NOT NULL,
  PRIMARY KEY (service, scope, target));

CREATE TABLE uninstall_intents (
  service    text,
  node_id    text,
  generation bigint NOT NULL,
  PRIMARY KEY (service, node_id));

CREATE TABLE inventory (
  host_id   text PRIMARY KEY,
  role      text NOT NULL,
  data_macs text[] NOT NULL,
  bmc_mac   text NOT NULL,
  serial    text NOT NULL,
  notes     text NOT NULL,
  by_name   text NOT NULL,
  at_unix   bigint NOT NULL,
  position  bigint NOT NULL,
  approve_until_unix bigint NOT NULL DEFAULT 0,   -- the record's approval window; 0 for none
  used_request_id    text,                        -- the request it approved, once used
  used_unix          bigint,
  CHECK ((used_request_id IS NULL) = (used_unix IS NULL)));
CREATE UNIQUE INDEX inventory_serial ON inventory (lower(serial)) WHERE serial <> '';
CREATE UNIQUE INDEX inventory_bmc ON inventory (lower(bmc_mac)) WHERE bmc_mac <> '';

-- Halts and plan positions (section 3.4).
CREATE TABLE halts (
  service    text PRIMARY KEY,
  node_id    text NOT NULL,
  check_name text NOT NULL,
  target     text NOT NULL,
  origin     text NOT NULL CHECK (origin IN ('hook', 'updater', 'operator')),
  report     jsonb);

CREATE TABLE halt_reports (
  service  text,
  node_id  text,
  kind     text CHECK (kind IN ('acked', 'seen')),
  target   text NOT NULL,
  report   jsonb,
  position bigint NOT NULL,
  PRIMARY KEY (service, kind, node_id));

CREATE TABLE rollouts (
  service       text PRIMARY KEY,
  version       text,
  entered       bigint NOT NULL DEFAULT 0,
  paused_by     text,
  paused_reason text,
  CHECK ((paused_by IS NULL) = (paused_reason IS NULL)));

CREATE TABLE rollout_done (
  service text REFERENCES rollouts ON DELETE CASCADE,
  node_id text,
  PRIMARY KEY (service, node_id));

-- The audit log (section 3.5): append-only, numbered by the coordinator.
CREATE TABLE audit (
  seq     bigint PRIMARY KEY,
  at_unix bigint NOT NULL,
  by_name text NOT NULL,
  action  text NOT NULL,
  service text,
  node_id text,
  reason  text);
CREATE INDEX audit_by ON audit (by_name, seq);
CREATE INDEX audit_action ON audit (action, seq);
CREATE INDEX audit_service ON audit (service, seq) WHERE service IS NOT NULL;
CREATE INDEX audit_node ON audit (node_id, seq) WHERE node_id IS NOT NULL;
CREATE INDEX audit_time ON audit (at_unix);

CREATE FUNCTION audit_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'the audit table is append-only: % is refused', TG_OP
    USING ERRCODE = '55000';
END
$$;
CREATE TRIGGER audit_rows_stay BEFORE UPDATE OR DELETE ON audit
  FOR EACH ROW EXECUTE FUNCTION audit_append_only();
CREATE TRIGGER audit_table_stays BEFORE TRUNCATE ON audit
  FOR EACH STATEMENT EXECUTE FUNCTION audit_append_only();

-- Spent CI token ids and the verdicts (section 3.6).
CREATE TABLE spent_tokens (
  jti        text PRIMARY KEY,
  until_unix bigint NOT NULL);
CREATE INDEX spent_tokens_until ON spent_tokens (until_unix);

CREATE TABLE verdict_standing (
  service text,
  node_id text,
  report  jsonb NOT NULL,
  PRIMARY KEY (service, node_id));

CREATE TABLE verdict_counted (
  service  text,
  position bigint,
  node_id  text NOT NULL,
  report   jsonb NOT NULL,
  PRIMARY KEY (service, position));

-- The enrollment store (section 3.6): the fields of its protobuf messages, one column each.
CREATE TABLE enrollment_requests (
  request_id          bytea PRIMARY KEY,
  state               smallint NOT NULL,
  expired_unix        bigint NOT NULL DEFAULT 0,
  expired_after_secs  bigint NOT NULL DEFAULT 0,
  posted_unix         bigint NOT NULL,
  round               bigint NOT NULL,
  serial              bigint NOT NULL DEFAULT 0,
  renewal_of          text NOT NULL DEFAULT '',
  image               text NOT NULL,
  csr_der             bytea NOT NULL,
  spki_der            bytea NOT NULL,
  mac_nonce_presented boolean NOT NULL,
  machine_id          text NOT NULL,
  dmi_serial          text NOT NULL,
  dmi_product_uuid    text NOT NULL,
  addresses           text[] NOT NULL,
  mac_serial          text NOT NULL,
  mac_platform_uuid   text NOT NULL,
  bmc_mac             text NOT NULL,
  approved_host_id    text,
  approved_role       text,
  approved_services   text[],
  approved_replace    boolean,
  approver            text,
  approved_unix       bigint,
  approved_reason     text,
  approved_mac_nonce  bytea);
CREATE INDEX enrollment_waiting ON enrollment_requests (posted_unix, request_id) WHERE state IN (1, 2);
CREATE INDEX enrollment_machine ON enrollment_requests (machine_id) WHERE machine_id <> '';

CREATE TABLE enrollment_issued (
  request_id      bytea PRIMARY KEY REFERENCES enrollment_requests DEFERRABLE INITIALLY DEFERRED,
  round           bigint NOT NULL,
  serial          bigint NOT NULL,
  host_id         text NOT NULL,
  not_after       bigint NOT NULL,
  certificate_der bytea NOT NULL);

CREATE TABLE enrollment_hosts (
  host_id              text PRIMARY KEY,
  key_hash             bytea NOT NULL,
  machine_id           text NOT NULL,
  role                 text NOT NULL,
  services             text[] NOT NULL,
  duplicate            boolean NOT NULL,
  first_heartbeat_unix bigint);

CREATE TABLE enrollment_deny (
  host_id    text PRIMARY KEY,
  state      smallint NOT NULL,
  cleared_by bytea);

CREATE TABLE enrollment_serial (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  next      bigint NOT NULL);
INSERT INTO enrollment_serial (next) VALUES (2);

CREATE TABLE mac_nonces (
  nonce_sha256 bytea PRIMARY KEY,
  serial       text NOT NULL,
  issued_unix  bigint NOT NULL,
  consumed_by  bytea,
  request_id   bytea);

-- Bookkeeping (section 3.7). schema_version is made by the runner before the first migration.
CREATE TABLE lease (
  singleton  boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  epoch      bigint NOT NULL,
  commits    bigint NOT NULL,
  holder     text NOT NULL,
  since_unix bigint NOT NULL);
INSERT INTO lease (epoch, commits, holder, since_unix) VALUES (0, 0, '', 0);

-- The fence (section 5): every transaction of the coordinator moves lease.commits on, and only
-- the session that holds the coordinator's advisory lock of this database may. The key is the
-- store's constant 0x686b_6370_6731: classid its upper 32 bits, objid its lower.
CREATE FUNCTION lease_fence() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS (
      SELECT 1 FROM pg_locks
      WHERE locktype = 'advisory' AND pid = pg_backend_pid() AND granted
        AND classid = 26731 AND objid = 1668310833 AND objsubid = 1) THEN
    RAISE EXCEPTION 'this session does not hold the coordinator''s lock of the database'
      USING ERRCODE = '55000';
  END IF;
  RETURN NEW;
END
$$;
CREATE TRIGGER lease_fenced BEFORE UPDATE ON lease
  FOR EACH ROW EXECUTE FUNCTION lease_fence();

CREATE TABLE imports (
  id                  bigint PRIMARY KEY,
  at_unix             bigint NOT NULL,
  manifest            jsonb NOT NULL,
  audit_pruned_before bigint);
