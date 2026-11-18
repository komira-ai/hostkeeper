//! What ties the models to the spec and to the tests.
//!
//! Each model file gives a [`ModelDef`]: its name, its bound, the invariants it checks and its
//! variants, each variant with the invariant it breaks and the smallest number of instances at
//! which it does (or `None`: a variant that removes a lock the design does not rely on alone,
//! which must still hold). [`case`] is the body of the test of one invariant; [`compare`] holds
//! the spec, `docs/design/models/coordinator-multi-instance.md`, to the definitions.

use std::collections::BTreeSet;

use crate::explore::Outcome;

/// The numbers of instances every model is run at. A symmetry argument carries the result to
/// any N: no step names an instance, and instances are interchangeable (the spec says why).
pub const INSTANCES: [usize; 3] = [1, 2, 3];

/// A variant of a model with one lock or one conditional write taken away (or replaced by what
/// the design rejects), and what that does.
pub struct VariantCase {
    pub variant: &'static str,
    /// The invariant the variant is checked against.
    pub breaks: &'static str,
    /// The smallest N of [`INSTANCES`] at which the invariant is broken; below it, it holds.
    /// `None`: it holds at every N (a backstop the design keeps on purpose).
    pub fails_from: Option<usize>,
}

/// One model: the definitions the spec is compared with, and the search.
pub struct ModelDef {
    pub name: &'static str,
    /// The bound of the script, in words, as the spec states it.
    pub bound: &'static str,
    pub invariants: &'static [&'static str],
    pub cases: &'static [VariantCase],
    /// The search at `n` instances, of the locked protocol (`None`) or of a variant, checking
    /// every invariant or only `only`.
    pub run: fn(n: usize, variant: Option<&'static str>, only: Option<&'static str>) -> Outcome,
}

/// The test of the invariant `name`: the locked protocol of its model keeps every invariant of
/// the model at N = 1, 2 and 3, and each variant checked against `name` breaks it exactly from
/// its `fails_from` on (and keeps it below).
pub fn case(models: &[&ModelDef], name: &'static str) {
    let owners: Vec<&&ModelDef> = models
        .iter()
        .filter(|m| m.invariants.contains(&name))
        .collect();
    assert_eq!(
        owners.len(),
        1,
        "the invariant {name} must belong to exactly one model"
    );
    let model = owners[0];
    for n in INSTANCES {
        let outcome = (model.run)(n, None, None);
        println!(
            "{} at N = {n}, locked: {} states",
            model.name, outcome.states
        );
        if let Some(found) = outcome.found {
            panic!(
                "model {} at N = {n}: the locked protocol breaks {}: {}\n{}",
                model.name,
                found.invariant,
                found.why,
                found.trace.join("\n")
            );
        }
    }
    let variants: Vec<&VariantCase> = model.cases.iter().filter(|c| c.breaks == name).collect();
    assert!(
        variants.iter().any(|c| c.fails_from.is_some()),
        "the invariant {name} has no variant that breaks it"
    );
    for c in variants {
        for n in INSTANCES {
            let outcome = (model.run)(n, Some(c.variant), Some(name));
            println!(
                "{} at N = {n}, {}: {} states, {}",
                model.name,
                c.variant,
                outcome.states,
                outcome
                    .found
                    .as_ref()
                    .map_or("holds".to_string(), |f| format!(
                        "broken after {} steps:\n  {}",
                        f.trace.len() - 1,
                        f.trace.join("\n  ")
                    ))
            );
            let expected = c.fails_from.is_some_and(|from| n >= from);
            match (&outcome.found, expected) {
                (Some(found), true) => assert_eq!(found.invariant, name),
                (None, false) => {}
                (Some(found), false) => panic!(
                    "model {} at N = {n}: the variant {} breaks {name} below N = {:?}: {}\n{}",
                    model.name,
                    c.variant,
                    c.fails_from,
                    found.why,
                    found.trace.join("\n")
                ),
                (None, true) => panic!(
                    "model {} at N = {n}: the variant {} keeps {name} over {} states, \
                     but the spec says it breaks it from N = {:?}",
                    model.name, c.variant, outcome.states, c.fails_from
                ),
            }
        }
    }
}

/// One row of a table of the spec: its cells, trimmed, without the backticks around a name.
fn rows<'a>(spec: &'a str, heading: &str) -> Result<Vec<Vec<&'a str>>, String> {
    let start = spec
        .find(&format!("\n## {heading}\n"))
        .ok_or_else(|| format!("the spec has no section `## {heading}`"))?;
    let body = &spec[start + heading.len() + 5..];
    let body = &body[..body.find("\n## ").unwrap_or(body.len())];
    Ok(body
        .lines()
        .filter(|l| l.starts_with('|'))
        // The header row and the separator row.
        .skip(2)
        .map(|l| {
            l.trim_matches('|')
                .split('|')
                .map(|c| c.trim().trim_matches('`'))
                .collect()
        })
        .collect())
}

/// The spec's tables against the models: every invariant, variant and bound the spec states is
/// the one a model defines, and the other way round; every invariant has a test (`tests`, the
/// names the test file defines a test for); every invariant is broken by some variant.
pub fn compare(spec: &str, models: &[&ModelDef], tests: &[&str]) -> Result<(), String> {
    let mut problems = Vec::new();

    let stated: BTreeSet<(String, String)> = rows(spec, "Invariants")?
        .iter()
        .map(|r| (r[0].to_string(), r.get(1).unwrap_or(&"").to_string()))
        .collect();
    let defined: BTreeSet<(String, String)> = models
        .iter()
        .flat_map(|m| {
            m.invariants
                .iter()
                .map(|i| (i.to_string(), m.name.to_string()))
        })
        .collect();
    differ(
        "an invariant (name, model)",
        &stated,
        &defined,
        &mut problems,
    );

    let stated: BTreeSet<(String, String, String, String)> = rows(spec, "Variants")?
        .iter()
        .map(|r| {
            let cell = |i: usize| r.get(i).unwrap_or(&"").to_string();
            (cell(0), cell(1), cell(2), cell(3))
        })
        .collect();
    let defined: BTreeSet<(String, String, String, String)> = models
        .iter()
        .flat_map(|m| {
            m.cases.iter().map(|c| {
                (
                    c.variant.to_string(),
                    m.name.to_string(),
                    c.breaks.to_string(),
                    c.fails_from.map_or("never".to_string(), |n| n.to_string()),
                )
            })
        })
        .collect();
    differ(
        "a variant (name, model, breaks, fails from N)",
        &stated,
        &defined,
        &mut problems,
    );

    let stated: BTreeSet<(String, String)> = rows(spec, "Bounds")?
        .iter()
        .map(|r| (r[0].to_string(), r.get(1).unwrap_or(&"").to_string()))
        .collect();
    let defined: BTreeSet<(String, String)> = models
        .iter()
        .map(|m| (m.name.to_string(), m.bound.to_string()))
        .collect();
    differ("a bound (model, bound)", &stated, &defined, &mut problems);

    let invariants: BTreeSet<&str> = models
        .iter()
        .flat_map(|m| m.invariants.iter().copied())
        .collect();
    let tested: BTreeSet<&str> = tests.iter().copied().collect();
    for i in invariants.difference(&tested) {
        problems.push(format!("the invariant {i} has no test"));
    }
    for t in tested.difference(&invariants) {
        problems.push(format!("the test {t} names no invariant"));
    }
    for m in models {
        for i in m.invariants {
            if !m
                .cases
                .iter()
                .any(|c| c.breaks == *i && c.fails_from.is_some())
            {
                problems.push(format!("the invariant {i} has no variant that breaks it"));
            }
        }
        for c in m.cases {
            if !m.invariants.contains(&c.breaks) {
                problems.push(format!(
                    "the variant {} of {} names {}, not an invariant of that model",
                    c.variant, m.name, c.breaks
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

/// Adds a line for each row only one side has.
fn differ<T: Ord + std::fmt::Debug>(
    what: &str,
    stated: &BTreeSet<T>,
    defined: &BTreeSet<T>,
    problems: &mut Vec<String>,
) {
    for row in stated.difference(defined) {
        problems.push(format!("the spec states {what} no model defines: {row:?}"));
    }
    for row in defined.difference(stated) {
        problems.push(format!(
            "a model defines {what} the spec does not state: {row:?}"
        ));
    }
}
