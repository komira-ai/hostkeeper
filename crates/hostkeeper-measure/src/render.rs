//! An analysis written out: a section of the results page (markdown) and a result file of
//! `docs/report/results` (tab-separated, the columns `kind`, `id`, `value`, `unit`, `how` and
//! `cells` of the directory's README). Neither has a figure; both name the plan and the run in
//! every number's line.

use crate::analysis::{Analysis, Clock, Measured};
use crate::template::{self, Piece};

/// A number as the page and the file print it: at most three decimals, no trailing zero.
pub fn number(v: f64) -> String {
    let text = format!("{v:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    match text {
        "-0" => "0".to_owned(),
        other => other.to_owned(),
    }
}

fn clocks(m: &Measured) -> String {
    let names: Vec<&str> = m.clocks().into_iter().map(Clock::name).collect();
    if names.is_empty() {
        "-".to_owned()
    } else {
        names.join(", ")
    }
}

/// The value of `{measure.stat}`, `{plan}` or `{run}` in a statement.
fn stat(a: &Analysis, slot: &str) -> String {
    match slot {
        "plan" => return a.plan.id.clone(),
        "run" => return a.run.clone(),
        _ => {}
    }
    let (id, which) = slot.split_once('.').unwrap_or((slot, ""));
    let Some(m) = a.results.iter().find(|m| m.measure.id == id) else {
        return format!("{{{slot}}}");
    };
    let stats = m.stats();
    match (which, stats) {
        ("n", _) => m.values.len().to_string(),
        ("missing", _) => m.missing.to_string(),
        ("min", Some(s)) => number(s.min),
        ("median", Some(s)) => number(s.median),
        ("max", Some(s)) => number(s.max),
        _ => "no value".to_owned(),
    }
}

/// A statement of the plan with its slots filled.
pub fn statement(a: &Analysis, text: &str) -> String {
    match template::pieces(text) {
        Ok(pieces) => pieces
            .into_iter()
            .map(|p| match p {
                Piece::Text(t) => t,
                Piece::Slot(s) => stat(a, &s),
            })
            .collect(),
        Err(_) => text.to_owned(),
    }
}

/// The section of the results page for one run.
pub fn markdown(a: &Analysis) -> String {
    let p = &a.plan;
    let mut out = format!(
        "## {}\n\nPlan [`{}`](../../../crates/hostkeeper-measure/plans/{}.toml), run `{}`: {} of {} repetitions started, {} complete.\n\n{}\n\n**Hosts.** {}\n\n**Recorded.** {}\n\n",
        p.title,
        p.id,
        p.id,
        a.run,
        a.started,
        p.repetitions,
        a.complete,
        p.measures.trim(),
        p.hosts.says.trim(),
        p.records.trim(),
    );
    out.push_str("| Measure | What | Unit | n | Missing | Min | Median | Max | Clock |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for m in &a.results {
        let (min, median, max) = match m.stats() {
            Some(s) => (number(s.min), number(s.median), number(s.max)),
            None => ("-".to_owned(), "-".to_owned(), "-".to_owned()),
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {min} | {median} | {max} | {} |\n",
            m.measure.id,
            m.measure.says,
            m.measure.unit,
            m.values.len(),
            m.missing,
            clocks(m),
        ));
    }
    out.push_str(
        "\nA coordinator time is the difference of two of the coordinator's stamps, in whole seconds, so within a second either way. A runner time is the difference of the runner's clock at the polls that saw the two events, each up to one poll interval after the event.\n\n**What the report states from it.**\n\n",
    );
    for s in &p.states {
        out.push_str(&format!("- {}\n", statement(a, s)));
    }
    out.push_str("\n**Repetitions that ended early.**");
    if a.ended.is_empty() {
        out.push_str(" None.\n");
    } else {
        out.push('\n');
        out.push('\n');
        for (rep, why) in &a.ended {
            out.push_str(&format!("- repetition {rep}: {why}\n"));
        }
    }
    out.push_str(&format!(
        "\n### Values of plan `{}`, run `{}`\n\n| Measure | Repetition | Host | Value | Unit | Clock |\n|---|---|---|---|---|---|\n",
        p.id, a.run
    ));
    for m in &a.results {
        for v in &m.values {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} |\n",
                m.measure.id,
                v.rep,
                v.host.as_deref().unwrap_or("-"),
                number(v.value),
                m.measure.unit,
                v.clock.map_or("-", Clock::name),
            ));
        }
    }
    out
}

/// The result file of one run.
pub fn tsv(a: &Analysis) -> String {
    let p = &a.plan;
    let mut out = format!(
        "# Plan {}, run {}: {}. Written by `hostkeeper-measure analyze` from the run's records.\n# A raw row is one value of a measure; a derived row is computed from the raw rows of its measure,\n# as its how column says. A time is the difference of two stamps of the one clock its how column\n# names: the coordinator's (whole seconds) or the runner's (up to one poll interval late).\n# kind\tid\tvalue\tunit\thow\tcells\n",
        p.id, a.run, p.title
    );
    let run = format!("plan {}, run {}", p.id, a.run);
    for m in &a.results {
        let id = &m.measure.id;
        for v in &m.values {
            let host = v.host.as_ref().map(|h| format!("-{h}")).unwrap_or_default();
            let host_how = v
                .host
                .as_ref()
                .map(|h| format!(", {h}"))
                .unwrap_or_default();
            let clock = v
                .clock
                .map(|c| format!("; {} clock", c.name()))
                .unwrap_or_default();
            out.push_str(&format!(
                "raw\t{id}-rep-{}{host}\t{}\t{}\t{run}, repetition {}{host_how}: {}{clock}\t\n",
                v.rep,
                number(v.value),
                m.measure.unit,
                v.rep,
                m.measure.says,
            ));
        }
        out.push_str(&format!(
            "derived\t{id}-n\t{}\t\t{run}: the number of raw rows of {id}\t\n",
            m.values.len()
        ));
        out.push_str(&format!(
            "derived\t{id}-missing\t{}\t\t{run}: the values of {id} the repetitions did not record\t\n",
            m.missing
        ));
        if let Some(s) = m.stats() {
            for (name, value, how) in [
                ("min", s.min, "the least"),
                ("median", s.median, "the median"),
                ("max", s.max, "the greatest"),
            ] {
                out.push_str(&format!(
                    "derived\t{id}-{name}\t{}\t{}\t{run}: {how} raw value of {id}\t\n",
                    number(value),
                    m.measure.unit
                ));
            }
        }
    }
    out
}
