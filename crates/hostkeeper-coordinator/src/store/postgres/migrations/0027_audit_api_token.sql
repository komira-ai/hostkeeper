-- older-releases: may-run
-- The API token an operator's change was made with (docs/design/operator-login.md, section 7.2):
-- an API token acts as the user who owns it, so the line's `by_name` is the owner and this column
-- the token, by name. NULL for a line made with a session, a token of the tokens file or by no
-- operator, and for every line written before.
--
-- A release before names the columns it writes and reads, so it runs on a database that has this
-- one and writes NULL in it. A shipped migration is never edited: its SHA-256 is pinned by a test.
ALTER TABLE audit ADD COLUMN api_token text;
