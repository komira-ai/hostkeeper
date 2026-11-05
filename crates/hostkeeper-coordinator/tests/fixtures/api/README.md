# The two forms of a JSON update answer

Goldens of the answer to a heartbeat (`POST /v1/heartbeat`) that the rollout gate answers with an update of `web` to `2.0`, whose package digest CI registered as `ab` repeated 32 times. `tests/api_compat.rs` checks the coordinator writes each file's bytes (without the trailing newline) and that the wire's `Answer` reads each back to the same bytes.

- `answer-update-plain.json`: to an agent that announces no feature (kbf's agent). These are the bytes this route has always written. They must not change.
- `answer-update-order-v1.json`: to an agent that announces `order-v1`. The order's terms (`order_id`, `order_try`, `package_digest`) come right after the reply's keys, then `features`. The order id is `update-` and the first 32 hex digits of the SHA-256 of `web\n2.0`.

A file here changes only with a change of the wire that is additive (AGENTS.md B8), in the same pull request as that change.
