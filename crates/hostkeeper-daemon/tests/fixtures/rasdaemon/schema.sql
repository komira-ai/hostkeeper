-- The tables of rasdaemon's database (/var/lib/rasdaemon/ras-mc_event.db) as rasdaemon makes them
-- (`CREATE TABLE IF NOT EXISTS <table> (<column> <type>, ...)` of its ras-record.c): the five the
-- daemon reads, and one it does not (devlink_event), which it passes over.
CREATE TABLE IF NOT EXISTS mc_event (id INTEGER PRIMARY KEY, timestamp TEXT, err_count INTEGER, err_type TEXT, err_msg TEXT, label TEXT, mc INTEGER, top_layer INTEGER, middle_layer INTEGER, lower_layer INTEGER, address INTEGER, grain INTEGER, syndrome INTEGER, driver_detail TEXT);
CREATE TABLE IF NOT EXISTS aer_event (id INTEGER PRIMARY KEY, timestamp TEXT, dev_name TEXT, err_type TEXT, err_msg TEXT);
CREATE TABLE IF NOT EXISTS mce_record (id INTEGER PRIMARY KEY, timestamp TEXT, mcgcap INTEGER, mcgstatus INTEGER, status INTEGER, addr INTEGER, misc INTEGER, ip INTEGER, tsc INTEGER, walltime INTEGER, cpu INTEGER, cpuid INTEGER, apicid INTEGER, socketid INTEGER, cs INTEGER, bank INTEGER, cpuvendor INTEGER, bank_name TEXT, error_msg TEXT, mcgstatus_msg TEXT, mcistatus_msg TEXT, mcastatus_msg TEXT, user_action TEXT, mc_location TEXT);
CREATE TABLE IF NOT EXISTS mf_event (id INTEGER PRIMARY KEY, timestamp TEXT, pfn TEXT, page_type TEXT, action_result TEXT);
CREATE TABLE IF NOT EXISTS disk_errors (id INTEGER PRIMARY KEY, timestamp TEXT, dev TEXT, sector INTEGER, nr_sector INTEGER, error TEXT, rwbs TEXT, cmd TEXT);
CREATE TABLE IF NOT EXISTS devlink_event (id INTEGER PRIMARY KEY, timestamp TEXT, bus_name TEXT, dev_name TEXT, driver_name TEXT, reporter_name TEXT, msg TEXT);
