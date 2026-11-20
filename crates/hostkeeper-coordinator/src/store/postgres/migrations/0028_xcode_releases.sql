-- older-releases: may-run
-- The Xcode releases the list at --xcode-feed-url last named (docs/design/maintenance-updates.md,
-- section 1.1): one row for each release, beta and release candidate, by its version, its build and
-- its channel (`release`, `beta` or `rc`), with the macOS it requires. The job `xcode-feed` replaces
-- the table whole after each read of the list that succeeds, in one transaction; nothing else
-- writes it, and the rows can be made again from the list, so an export does not carry them.
--
-- No release before reads or writes this table, so it runs on a database that has it. A shipped
-- migration is never edited: its SHA-256 is pinned by a test.
CREATE TABLE xcode_releases (
    version text NOT NULL,
    build text NOT NULL,
    channel text NOT NULL CHECK (channel IN ('release', 'beta', 'rc')),
    requires_macos text NOT NULL,
    PRIMARY KEY (version, build, channel)
);
