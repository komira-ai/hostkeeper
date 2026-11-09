-- The rows of each table, printed as `hostkeeper-coordinator export` prints them, for the drill
-- (README.md, "The restore drill"). Run on the live database with `psql --quiet --tuples-only
-- --no-align --set=ON_ERROR_STOP=1 --file=/etc/hostkeeper-db/counts.sql`, it reads the counts
-- without the coordinator's lock, so the coordinator keeps running. Like export, it counts every
-- row, the spent CI token ids already past among them.
SET search_path = hostkeeper;
SELECT format('  %s: %s rows', name, rows) FROM (VALUES
  (1, 'desired', (SELECT count(*) FROM desired)),
  (2, 'desired_digests', (SELECT count(*) FROM desired_digests)),
  (3, 'releases', (SELECT count(*) FROM releases)),
  (4, 'services', (SELECT count(*) FROM services)),
  (5, 'bad_versions', (SELECT count(*) FROM bad_versions)),
  (6, 'failure_acks', (SELECT count(*) FROM failure_acks)),
  (7, 'host_labels', (SELECT count(*) FROM host_labels)),
  (8, 'exclusions', (SELECT count(*) FROM exclusions)),
  (9, 'holds', (SELECT count(*) FROM holds)),
  (10, 'assignments', (SELECT count(*) FROM assignments)),
  (11, 'min_versions', (SELECT count(*) FROM min_versions)),
  (12, 'uninstall_intents', (SELECT count(*) FROM uninstall_intents)),
  (13, 'inventory', (SELECT count(*) FROM inventory)),
  (14, 'halts', (SELECT count(*) FROM halts)),
  (15, 'halt_reports', (SELECT count(*) FROM halt_reports)),
  (16, 'rollouts', (SELECT count(*) FROM rollouts)),
  (17, 'rollout_done', (SELECT count(*) FROM rollout_done)),
  (18, 'audit', (SELECT count(*) FROM audit)),
  (19, 'spent_tokens', (SELECT count(*) FROM spent_tokens)),
  (20, 'verdict_standing', (SELECT count(*) FROM verdict_standing)),
  (21, 'verdict_counted', (SELECT count(*) FROM verdict_counted)),
  (22, 'enrollment_requests', (SELECT count(*) FROM enrollment_requests)),
  (23, 'enrollment_issued', (SELECT count(*) FROM enrollment_issued)),
  (24, 'enrollment_hosts', (SELECT count(*) FROM enrollment_hosts)),
  (25, 'enrollment_host_running', (SELECT count(*) FROM enrollment_host_running)),
  (26, 'enrollment_deny', (SELECT count(*) FROM enrollment_deny)),
  (27, 'mac_nonces', (SELECT count(*) FROM mac_nonces)),
  (28, 'mdm_devices', (SELECT count(*) FROM mdm_devices))
) AS counted (n, name, rows) ORDER BY n;
