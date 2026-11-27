-- older-releases: may-run
-- Each host's hardware event log (docs/design/host-health-alerts.md, section 2): one row per
-- record, keyed by the host and the record's `event_id`, written by a conditional insert on that
-- key, so a record a daemon sends again or that two coordinators receive is one row. A row is never
-- changed or removed: a trigger refuses an UPDATE, a DELETE and a TRUNCATE. The component,
-- severity and state are tokens (`memory`, `warning`, `asserted`) with no CHECK, so a later
-- release may write a token this one does not know, which it answers as unspecified. The keys are
-- compared byte by byte (`COLLATE "C"`), the order the files backend gives the log too.
--
-- A release before has no such table and never reads it; it runs on a database that holds it and
-- leaves its rows as they are.
-- A shipped migration is never edited: its SHA-256 is pinned by a test.
CREATE TABLE hardware_events (
  host_id          text COLLATE "C" NOT NULL,
  event_id         text COLLATE "C" NOT NULL CHECK (octet_length(event_id) BETWEEN 1 AND 64),
  machine_serial   text NOT NULL DEFAULT '',
  time_unix        bigint NOT NULL CHECK (time_unix >= 0),
  observed_unix    bigint NOT NULL CHECK (observed_unix >= 0),
  boot_id          text NOT NULL DEFAULT '',
  of_previous_boot boolean NOT NULL DEFAULT false,
  component        text NOT NULL,
  subject          text NOT NULL DEFAULT '',
  severity         text NOT NULL,
  state            text NOT NULL,
  source           text NOT NULL,
  code             text NOT NULL,
  message          text NOT NULL,
  raw              text NOT NULL DEFAULT '',
  about            text NOT NULL DEFAULT '',
  by_name          text NOT NULL DEFAULT '',
  PRIMARY KEY (host_id, event_id));
CREATE INDEX hardware_events_time ON hardware_events (time_unix, host_id, event_id);
CREATE INDEX hardware_events_serial ON hardware_events (machine_serial, time_unix);

CREATE FUNCTION hardware_events_append_only() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  RAISE EXCEPTION 'the hardware event log is append-only: % is refused', TG_OP
    USING ERRCODE = '55000';
END
$$;
CREATE TRIGGER hardware_events_rows_stay BEFORE UPDATE OR DELETE ON hardware_events
  FOR EACH ROW EXECUTE FUNCTION hardware_events_append_only();
CREATE TRIGGER hardware_events_table_stays BEFORE TRUNCATE ON hardware_events
  FOR EACH STATEMENT EXECUTE FUNCTION hardware_events_append_only();
