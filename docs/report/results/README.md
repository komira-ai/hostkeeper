# Result files of the technical report

Each measured cell of [the report](https://github.com/komira-ai/hostkeeper/blob/main/docs/report/hostkeeper.html)
names a file of this directory in its `data-result` attribute, and every number the cell prints is
the value of a row of that file that names the cell. The check is `cargo test -p xtask --test report`.

A result file is tab-separated text with the columns `kind`, `id`, `value`, `unit`, `how` and
`cells`. Lines starting with `#` are comments. `value` is a number. `how` says how the row was
obtained (the run and the command, or the arithmetic of a derived row). `cells` lists, separated by
commas, every cell id that prints the value; a row no cell prints leaves it out.

`farm-canary-node-5.tsv` holds the first farm canary: one run of each step on one Linux node of
the lab, node-5. The report's measurements that run did not take are open cells. The check
`canary_tests.rs` (in `crates/xtask/tests/report/`) holds that each row of that file is printed by
exactly the cells it names, and that a cell or a row changed alone fails the check.

`farm-rollout-linux.tsv` holds the rollout to the other five Linux nodes of the lab after that
canary: one `raw` row per node and measurement, which no cell prints, and `derived` rows (the least
and the greatest time, to 0.1 s, and the number of nodes) that the report's two rollout cells print.
The check `rollout_tests.rs` holds that each row is printed by exactly the cells it names, that
each derived row is what the raw rows give, and that a derived row they do not give fails.
