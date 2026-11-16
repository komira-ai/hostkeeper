-- older-releases: may-run
-- When a service in the rollout queue last asked for an order, so that a service that stopped
-- asking (its rollout over, its hosts gone, its version set back) leaves the places of the queue
-- by itself after the time the decision names, and no other service waits behind it for ever
-- (docs/design/coordinator-multi-instance.md, section 5.3). A release before this one never reads
-- the table.
ALTER TABLE rollout_queue ADD COLUMN asked_unix bigint NOT NULL DEFAULT 0;
