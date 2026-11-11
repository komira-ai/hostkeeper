//! The rules of the job [`JOB`] of `release.yml`, the check right before hostkeeper's own set
//! ([`super::desired::JOB`]) that no release newer than the run's is published. The job `targets`
//! checks that once, when it writes the targets; a re-run of an older release's failed jobs keeps
//! those outputs, so without a check made again it would set the older release after a newer one
//! was set, and move every host back. [`problems`] refuses `release.yml` when [`JOB`]:
//! - lacks the exact `if:` [`GUARD_IF`] (it runs exactly when the set would, but for its own
//!   answer), or has `continue-on-error`;
//! - does not need exactly the jobs of [`guard_needs`], every job the set needs but itself: so
//!   a re-run of any of them (with **Re-run failed jobs**) re-runs this job too, and the check is
//!   made again in that attempt;
//! - holds other token rights than exactly `contents: read`;
//! - has steps other than exactly [`STEPS`], in that order, with their `if:`, `with` and `env`
//!   ([`super::desired::step_shape`]): the published releases listed again, `set=true` only
//!   when none is higher than the run's version, and otherwise a warning that says the release
//!   is not set and why (each value through the step's `env:`, none written into a `run:`);
//! - has `outputs` other than exactly [`OUTPUTS`]: among them `attempt`, the attempt of the run
//!   the job ran in, which the set's `if:` ([`super::desired::SET_DESIRED_IF`]) compares with
//!   its own, so that a re-run of the set alone, after a check of an earlier attempt, is skipped.

use super::desired::{FILE, needs, pairs, step_shape, text};
use super::lint::entry;
use super::yaml::{Node, Value};

/// The job.
pub const JOB: &str = "no-downgrade";

/// The `if:` of [`JOB`]: the condition of the set without its own two terms on this job's
/// outputs ([`super::desired::SET_DESIRED_IF`]).
pub const GUARD_IF: &str = "${{ !cancelled() && !failure() && needs.plan.outputs.release == 'true' && github.ref == 'refs/heads/main' && vars.RELEASE_SETS_DESIRED == 'true' && needs.targets.outputs.newest == 'true' && needs.coordinator.result == 'success' && (needs.testing-apt-write.outputs.conflict == 'false' || needs.testing-apt-write-2.outputs.conflict == 'false' || needs.testing-apt-write-3.outputs.conflict == 'false') && needs.testing-pkg-upload.outputs.enabled == 'true' }}";

/// The terms the set's `if:` adds to [`GUARD_IF`]: this job said `set=true`, in the same
/// attempt of the run as the set.
pub const SET_TERMS: &str = " && needs.no-downgrade.outputs.set == 'true' && needs.no-downgrade.outputs.attempt == github.run_attempt";

/// The steps of [`JOB`], in order, as [`step_shape`] writes them.
pub const STEPS: [&str; 2] = [
    r#"run gh release list --repo "$GITHUB_REPOSITORY" --exclude-drafts --limit 100 --json tagName --jq '[.[].tagName | select(test("^v[0-9]+\\.[0-9]+\\.[0-9]+$")) | ltrimstr("v") | split(".") | map(tonumber)] | max | "newest=\(map(tostring) | join("."))", "set=\(. <= (env.VERSION | split(".") | map(tonumber)))"' >> "$GITHUB_OUTPUT" env GH_TOKEN=${{ github.token }} env VERSION=${{ needs.targets.outputs.version }}"#,
    r#"if ${{ steps.newest.outputs.set != 'true' }} run echo "::warning::Not setting hostkeeper $VERSION as the desired version on the coordinator: $NEWEST is published, a newer release, and setting $VERSION would move every host back. This is a run of an older release (a re-run of its jobs, or a late run); the newest release's own run sets the fleet. A move back is an operator's step on the coordinator, never this workflow's." env NEWEST=${{ steps.newest.outputs.newest }} env VERSION=${{ needs.targets.outputs.version }}"#,
];

/// The `outputs` of [`JOB`], exactly, sorted by name.
pub const OUTPUTS: [(&str, &str); 3] = [
    ("attempt", "${{ github.run_attempt }}"),
    ("newest", "${{ steps.newest.outputs.newest }}"),
    ("set", "${{ steps.newest.outputs.set }}"),
];

/// The jobs [`JOB`] needs, sorted: every job the set needs but [`JOB`] itself.
pub fn guard_needs() -> Vec<&'static str> {
    let mut want = super::desired::caller_needs();
    want.retain(|j| *j != JOB);
    want
}

/// One line per rule of this module that the workflow `doc` of `release.yml` breaks.
pub fn problems(doc: &Node) -> Vec<String> {
    let job = super::desired::job(doc, JOB);
    let mut problems = Vec::new();
    if text(job, "if") != Some(GUARD_IF) {
        problems.push(format!(
            "{FILE}: job {JOB} needs the exact `if: {GUARD_IF}`"
        ));
    }
    if job.and_then(|j| entry(j, "continue-on-error")).is_some() {
        problems.push(format!(
            "{FILE}: job {JOB} has `continue-on-error`: a failure must stop the deploy"
        ));
    }
    let got = needs(job);
    let want = guard_needs();
    if got != want {
        problems.push(format!("{FILE}: job {JOB} needs {got:?}, not {want:?}"));
    }
    if job.map(|j| pairs(j, "permissions")) != Some(vec![("contents", "read")]) {
        problems.push(format!(
            "{FILE}: job {JOB} must hold exactly `contents: read`"
        ));
    }
    let steps = match job.and_then(|j| entry(j, "steps")).map(|s| &s.value) {
        Some(Value::Seq(items)) => &items[..],
        _ => &[],
    };
    let shapes: Vec<String> = steps.iter().map(step_shape).collect();
    if shapes != STEPS {
        problems.push(format!(
            "{FILE}: job {JOB} steps are {shapes:?}, not no_downgrade::STEPS"
        ));
    }
    let outputs = job.map(|j| pairs(j, "outputs")).unwrap_or_default();
    if outputs != OUTPUTS {
        problems.push(format!(
            "{FILE}: job {JOB} outputs are {outputs:?}, not {OUTPUTS:?}"
        ));
    }
    problems
}
