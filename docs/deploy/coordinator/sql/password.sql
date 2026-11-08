-- Sets the password of the role hostkeeper to the secret hostkeeper-db-password, which psql reads
-- itself (the trailing newline dropped), so the password is never on a command line or in an
-- environment. Run by init.sql, and on its own to change the password (README.md, "Changing the
-- password").
\set password `cat /run/secrets/hostkeeper-db-password`
ALTER ROLE hostkeeper PASSWORD :'password';
