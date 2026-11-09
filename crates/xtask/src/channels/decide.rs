//! `channels decide`: whether a release that is in `testing` goes on to the public channels.
//!
//! The release was published to testing (the apt suite `testing`, the rpm tree `testing`, the
//! macOS installer's directory `pkg/<product>/<version>/` without the pointer) and installed
//! from there on clean machines. The job `promote-check` runs this rule over the results of
//! those jobs (`--result NAME=RESULT`, the `needs.<job>.result` of each), and `promote=true` is
//! what the jobs that make the release public wait for.
//!
//! The stages are the product's list ([`Product::stages`]) and the ones that may be skipped
//! ([`Product::may_skip`]):
//!
//! | stage | result that lets the release go on |
//! |---|---|
//! | `testing-apt-write`, `testing-rpm-write`, `testing-pkg-upload` | `success` (a job without its settings ends green) |
//! | `testing-apt-write-2`, `testing-apt-write-3`, `testing-rpm-write-2`, `testing-rpm-write-3` | `success`, or `skipped` (the first write did not lose the race) |
//! | `check-apt`, `check-rpm`, `check-pkg` | `success`, or `skipped` (nothing was published to check) |
//!
//! Every other result, a stage not given, and a result that is not one of `success`, `failure`,
//! `cancelled` and `skipped`, is a refusal that names the stage; the job ends red.
//!
//! Copied from komira-ai/tierstore at ff4777831c08fc13aa025b54e6e0531c2f350f4f
//! (`crates/xtask/src/channels/decide.rs`), with the stages made data ([`super::spec`]); the lab
//! stages (`farm-check`, `farm`, `soak`) and the Homebrew and `install.sh` checks are not part of
//! this product's release.

use std::path::Path;

use super::spec::Product;
use super::{Error, append_output};

/// The results `needs.<job>.result` takes.
const RESULTS: [&str; 4] = ["success", "failure", "cancelled", "skipped"];

fn parse<'a>(product: &Product, results: &'a [String]) -> Result<Vec<(&'a str, &'a str)>, Error> {
    let mut parsed: Vec<(&str, &str)> = Vec::new();
    for result in results {
        let Some((name, value)) = result.split_once('=') else {
            return Err(Error::Usage(format!(
                "--result {result:?} is not NAME=RESULT"
            )));
        };
        if !product.stages.contains(&name) {
            return Err(Error::Usage(format!(
                "--result names {name:?}, which is no stage"
            )));
        }
        if !RESULTS.contains(&value) {
            return Err(Error::Usage(format!(
                "--result {name} is {value:?}, not one of {RESULTS:?}"
            )));
        }
        if parsed.iter().any(|(n, _)| *n == name) {
            return Err(Error::Usage(format!("--result {name} is given twice")));
        }
        parsed.push((name, value));
    }
    if let Some(missing) = product
        .stages
        .iter()
        .find(|s| !parsed.iter().any(|(n, _)| n == *s))
    {
        return Err(Error::Usage(format!("--result {missing} is not given")));
    }
    Ok(parsed)
}

/// The `decide` verb: the lines to print.
///
/// # Errors
/// A flag out of its form, or one refusal per stage whose result does not let the release go on.
pub(crate) fn decide(
    product: &Product,
    results: &[String],
    output: Option<&Path>,
) -> Result<Vec<String>, Error> {
    let problems: Vec<String> = parse(product, results)?
        .into_iter()
        .filter(|(stage, result)| {
            !(*result == "success" || (*result == "skipped" && product.may_skip.contains(stage)))
        })
        .map(|(stage, result)| format!("{stage} is {result}: the release does not go on"))
        .collect();
    if !problems.is_empty() {
        return Err(Error::Refused(problems));
    }
    if let Some(output) = output {
        append_output(output, &["promote=true".to_owned()])?;
    }
    Ok(vec![
        "promote: the release is promoted from testing".to_owned(),
    ])
}

#[cfg(test)]
mod tests;
