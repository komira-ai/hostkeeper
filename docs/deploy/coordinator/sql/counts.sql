-- The rows of each table, printed as `hostkeeper-coordinator export` prints them, for the drill
-- (README.md, "The restore drill"). Run on the live database with `psql --quiet --tuples-only
-- --no-align --set=ON_ERROR_STOP=1 --file=/etc/hostkeeper-db/counts.sql`, it reads the counts
-- without the coordinator's lock, so the coordinator keeps running.
SET search_path = hostkeeper;
SELECT format('  %s: %s rows', name, rows) FROM (VALUES
  (1, 'desired', (SELECT count(*) FROM desired)),
  (2, 'desired_digests', (SELECT count(*) FROM desired_digests)),
  (3, 'services', (SELECT count(*) FROM services)),
  (4, 'bad_versions', (SELECT count(*) FROM bad_versions)),
  (5, 'failure_acks', (SELECT count(*) FROM failure_acks)),
  (6, 'host_labels', (SELECT count(*) FROM host_labels)),
  (7, 'exclusions', (SELECT count(*) FROM exclusions)),
  (8, 'holds', (SELECT count(*) FROM holds)),
  (9, 'assignments', (SELECT count(*) FROM assignments)),
  (10, 'min_versions', (SELECT count(*) FROM min_versions)),
  (11, 'uninstall_intents', (SELECT count(*) FROM uninstall_intents)),
  (12, 'inventory', (SELECT count(*) FROM inventory)),
  (13, 'halts', (SELECT count(*) FROM halts)),
  (14, 'halt_reports', (SELECT count(*) FROM halt_reports)),
  (15, 'rollouts', (SELECT count(*) FROM rollouts)),
  (16, 'rollout_done', (SELECT count(*) FROM rollout_done)),
  (17, 'audit', (SELECT count(*) FROM audit)),
  (18, 'spent_tokens', (SELECT count(*) FROM spent_tokens)),
  (19, 'verdict_standing', (SELECT count(*) FROM verdict_standing)),
  (20, 'verdict_counted', (SELECT count(*) FROM verdict_counted)),
  (21, 'enrollment_requests', (SELECT count(*) FROM enrollment_requests)),
  (22, 'enrollment_issued', (SELECT count(*) FROM enrollment_issued)),
  (23, 'enrollment_hosts', (SELECT count(*) FROM enrollment_hosts)),
  (24, 'enrollment_deny', (SELECT count(*) FROM enrollment_deny)),
  (25, 'mac_nonces', (SELECT count(*) FROM mac_nonces))
) AS counted (n, name, rows) ORDER BY n;
