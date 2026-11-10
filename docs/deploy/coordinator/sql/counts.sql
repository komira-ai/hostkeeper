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
  (4, 'repositories', (SELECT count(*) FROM repositories)),
  (5, 'repository_generation', (SELECT count(*) FROM repository_generation)),
  (6, 'services', (SELECT count(*) FROM services)),
  (7, 'bad_versions', (SELECT count(*) FROM bad_versions)),
  (8, 'failure_acks', (SELECT count(*) FROM failure_acks)),
  (9, 'host_labels', (SELECT count(*) FROM host_labels)),
  (10, 'exclusions', (SELECT count(*) FROM exclusions)),
  (11, 'holds', (SELECT count(*) FROM holds)),
  (12, 'assignments', (SELECT count(*) FROM assignments)),
  (13, 'min_versions', (SELECT count(*) FROM min_versions)),
  (14, 'uninstall_intents', (SELECT count(*) FROM uninstall_intents)),
  (15, 'inventory', (SELECT count(*) FROM inventory)),
  (16, 'halts', (SELECT count(*) FROM halts)),
  (17, 'halt_reports', (SELECT count(*) FROM halt_reports)),
  (18, 'rollouts', (SELECT count(*) FROM rollouts)),
  (19, 'rollout_done', (SELECT count(*) FROM rollout_done)),
  (20, 'audit', (SELECT count(*) FROM audit)),
  (21, 'spent_tokens', (SELECT count(*) FROM spent_tokens)),
  (22, 'verdict_standing', (SELECT count(*) FROM verdict_standing)),
  (23, 'verdict_counted', (SELECT count(*) FROM verdict_counted)),
  (24, 'enrollment_requests', (SELECT count(*) FROM enrollment_requests)),
  (25, 'enrollment_issued', (SELECT count(*) FROM enrollment_issued)),
  (26, 'enrollment_hosts', (SELECT count(*) FROM enrollment_hosts)),
  (27, 'enrollment_host_running', (SELECT count(*) FROM enrollment_host_running)),
  (28, 'enrollment_deny', (SELECT count(*) FROM enrollment_deny)),
  (29, 'mac_nonces', (SELECT count(*) FROM mac_nonces)),
  (30, 'mdm_devices', (SELECT count(*) FROM mdm_devices))
) AS counted (n, name, rows) ORDER BY n;
