-- Run once after a restore, before the coordinator starts (README.md, "Restore"): moves the
-- enrollment serial counter on by 1,048,576, so a certificate issued after the restore never takes
-- a serial that one issued after the dump already had.
UPDATE hostkeeper.enrollment_serial SET next = next + 1048576;
