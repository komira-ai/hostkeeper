# Result files of the technical report

Each measured cell of [the report](../hostkeeper.md) names a file of this directory in its
`data-result` attribute, and every number the cell prints is the value of a row of that file that
names the cell. The check is `cargo test -p xtask --test report`.

A result file is tab-separated text with the columns `kind`, `id`, `value`, `unit`, `how` and
`cells`. Lines starting with `#` are comments. `value` is a number. `how` says how the row was
obtained (the run and the command, or the arithmetic of a derived row). `cells` lists, separated by
commas, every cell id that prints the value; a row no cell prints leaves it out.

No measurement has been taken yet, so the directory holds no result file and every measurement
cell of the report is open.
