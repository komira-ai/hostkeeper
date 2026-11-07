-- The coordinator's role and database, made once (README.md, "One-time setup", step 3), by
-- `psql --username=postgres --set=ON_ERROR_STOP=1 --file=/etc/hostkeeper-db/init.sql` inside the
-- postgres container. The role logs in with the password of the secret hostkeeper-db-password
-- (password.sql) and owns the database; the coordinator's first start makes its schema in it.
CREATE ROLE hostkeeper LOGIN;
\ir password.sql
CREATE DATABASE hostkeeper OWNER hostkeeper;
