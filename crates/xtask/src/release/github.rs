//! The GitHub adapter: answers [`GitHubRuns`], [`ReleaseStore`] and [`Verifier`] by running `gh`
//! through an [`Exec`]. Every argument list and every parse of GitHub's JSON is here, so a test
//! scripts the [`Exec`] with realistic JSON and checks them all without a network.
//!
//! A command that cannot start or ends unsuccessfully, a reply that is not JSON, and a reply
//! that lacks a field the port needs are each a [`PortError`] whose `what` is the command line.

use std::path::Path;

use serde_json::Value;

use super::contract::{Commit, MAIN, REPO, Repo, SIGNER_REF, SIGNER_WORKFLOW};
use super::git::stdout_of;
use super::ports::{
    Attestation, Exec, GitHubRuns, Issue, PortError, Release, ReleaseStore, Run, StoredAsset,
    Verifier,
};

/// What `gh api` prints on stderr for a path GitHub does not have.
const NOT_FOUND: &str = "(HTTP 404)";

/// The start of the URI a signing certificate names the workflow by.
const SIGNER_URI_START: &str = "https://github.com/";

/// `gh` and its arguments as one command line text.
fn line(args: &[&str]) -> String {
    format!("gh {}", args.join(" "))
}

/// `path` as text, or the error of the command `command`, which cannot name it.
fn utf8_path<'p>(command: &str, path: &'p Path) -> Result<&'p str, PortError> {
    path.to_str()
        .ok_or_else(|| PortError::new(command, format!("the path {} is not UTF-8", path.display())))
}

/// Reads fields out of the JSON a command printed, and names the command in each error.
struct Reader<'a> {
    what: &'a str,
}

impl Reader<'_> {
    fn fail(&self, label: &str, wanted: &str) -> PortError {
        PortError::new(self.what, format!("{label} is missing or not {wanted}"))
    }

    fn string(&self, value: &Value, label: &str) -> Result<String, PortError> {
        value
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| self.fail(label, "a string"))
    }

    fn number(&self, value: &Value, label: &str) -> Result<u64, PortError> {
        value.as_u64().ok_or_else(|| self.fail(label, "a number"))
    }

    fn flag(&self, value: &Value, label: &str) -> Result<bool, PortError> {
        value
            .as_bool()
            .ok_or_else(|| self.fail(label, "true or false"))
    }

    fn list<'v>(&self, value: &'v Value, label: &str) -> Result<&'v Vec<Value>, PortError> {
        value.as_array().ok_or_else(|| self.fail(label, "a list"))
    }
}

/// One entry of `workflow_runs`.
fn parse_run(r: &Reader<'_>, run: &Value) -> Result<Run, PortError> {
    Ok(Run {
        id: r.number(&run["id"], "id")?,
        event: r.string(&run["event"], "event")?,
        branch: run["head_branch"].as_str().unwrap_or_default().to_owned(),
        head_sha: r.string(&run["head_sha"], "head_sha")?,
        status: r.string(&run["status"], "status")?,
        conclusion: run["conclusion"].as_str().map(str::to_owned),
    })
}

/// One entry of the issues list.
fn parse_issue(r: &Reader<'_>, issue: &Value) -> Result<Issue, PortError> {
    Ok(Issue {
        number: r.number(&issue["number"], "number")?,
        title: r.string(&issue["title"], "title")?,
        body: issue["body"].as_str().unwrap_or_default().to_owned(),
    })
}

/// One asset of a release; its digest is read from `sha256:<hex>`, none for any other text.
fn parse_asset(r: &Reader<'_>, asset: &Value) -> Result<StoredAsset, PortError> {
    Ok(StoredAsset {
        name: r.string(&asset["name"], "name")?,
        sha256: asset["digest"]
            .as_str()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .map(str::to_owned),
    })
}

/// One entry of the releases list.
fn parse_release(r: &Reader<'_>, release: &Value) -> Result<Release, PortError> {
    Ok(Release {
        tag: r.string(&release["tag_name"], "tag_name")?,
        commit: r.string(&release["target_commitish"], "target_commitish")?,
        draft: r.flag(&release["draft"], "draft")?,
        assets: r
            .list(&release["assets"], "assets")?
            .iter()
            .map(|asset| parse_asset(r, asset))
            .collect::<Result<Vec<StoredAsset>, PortError>>()?,
    })
}

/// One element of the array `gh attestation verify --format json` prints.
fn parse_attestation(r: &Reader<'_>, element: &Value) -> Result<Attestation, PortError> {
    let result = &element["verificationResult"];
    let certificate = &result["signature"]["certificate"];
    let uri = r.string(&certificate["buildSignerURI"], "buildSignerURI")?;
    let signer_workflow = uri
        .strip_prefix(SIGNER_URI_START)
        .and_then(|rest| rest.split_once('@'))
        .map(|(workflow, _ref)| workflow.to_owned())
        .ok_or_else(|| {
            PortError::new(
                r.what,
                format!("buildSignerURI {uri:?} is not {SIGNER_URI_START}<workflow>@<ref>"),
            )
        })?;
    Ok(Attestation {
        signer_workflow,
        source_ref: r.string(&certificate["sourceRepositoryRef"], "sourceRepositoryRef")?,
        source_commit: r.string(
            &certificate["sourceRepositoryDigest"],
            "sourceRepositoryDigest",
        )?,
        subjects: r
            .list(&result["statement"]["subject"], "subject")?
            .iter()
            .map(|subject| r.string(&subject["digest"]["sha256"], "subject sha256"))
            .collect::<Result<Vec<String>, PortError>>()?,
    })
}

/// The GitHub command line tool, run through an [`Exec`].
pub(crate) struct Gh<E> {
    exec: E,
}

impl<E: Exec> Gh<E> {
    pub(crate) fn new(exec: E) -> Gh<E> {
        Gh { exec }
    }

    /// What `gh <args>` printed on stdout.
    fn call(&self, args: &[&str]) -> Result<Vec<u8>, PortError> {
        let owned: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        stdout_of(&self.exec, "gh", &owned)
    }

    /// The JSON `gh <args>` printed, and the command line text for the errors of its reader.
    fn json(&self, args: &[&str]) -> Result<(Value, String), PortError> {
        let out = self.call(args)?;
        let what = line(args);
        let value = serde_json::from_slice(&out)
            .map_err(|e| PortError::new(what.clone(), format!("not JSON: {e}")))?;
        Ok((value, what))
    }

    /// The JSON of the GitHub API path `path`.
    fn api(&self, path: &str) -> Result<(Value, String), PortError> {
        self.json(&["api", path])
    }
}

impl<E: Exec> GitHubRuns for Gh<E> {
    /// A workflow file that is not on the default branch has no runs: GitHub answers the path
    /// with 404 Not Found, read here as an empty list, so that the plan waits for the workflow
    /// (a required one that is not there yet) instead of failing. That only ever holds a
    /// release back: no run, no release.
    fn runs(&self, repo: &Repo, workflow: &str, commit: &Commit) -> Result<Vec<Run>, PortError> {
        let path = format!(
            "repos/{repo}/actions/workflows/{workflow}/runs?head_sha={commit}&per_page=100"
        );
        let (value, what) = match self.api(&path) {
            Err(e) if e.why.contains(NOT_FOUND) => return Ok(Vec::new()),
            answer => answer?,
        };
        let r = Reader { what: &what };
        r.list(&value["workflow_runs"], "workflow_runs")?
            .iter()
            .map(|run| parse_run(&r, run))
            .collect()
    }

    fn on_main(&self, repo: &Repo, commit: &Commit) -> Result<bool, PortError> {
        let (value, what) = self.api(&format!("repos/{repo}/compare/{MAIN}...{commit}"))?;
        let status = Reader { what: &what }.string(&value["status"], "status")?;
        Ok(matches!(status.as_str(), "identical" | "behind"))
    }

    /// Every page of the list: an issue past the first hundred is still found, so a caller
    /// that looks for its one issue by title never opens a second. `--slurp` prints the pages
    /// as one list of lists.
    fn open_issues(&self, repo: &Repo) -> Result<Vec<Issue>, PortError> {
        let path = format!("repos/{repo}/issues?state=open&per_page=100");
        let (value, what) = self.json(&["api", "--paginate", "--slurp", &path])?;
        let r = Reader { what: &what };
        let mut issues = Vec::new();
        for page in r.list(&value, "the reply")? {
            for item in r.list(page, "a page")? {
                if item.get("pull_request").is_none() {
                    issues.push(parse_issue(&r, item)?);
                }
            }
        }
        Ok(issues)
    }

    fn open_issue(&self, repo: &Repo, title: &str, body: &str) -> Result<u64, PortError> {
        let path = format!("repos/{repo}/issues");
        let (value, what) = self.json(&[
            "api",
            "--method",
            "POST",
            &path,
            "-f",
            &format!("title={title}"),
            "-f",
            &format!("body={body}"),
        ])?;
        Reader { what: &what }.number(&value["number"], "number")
    }

    fn edit_issue(&self, repo: &Repo, number: u64, body: &str) -> Result<(), PortError> {
        let path = format!("repos/{repo}/issues/{number}");
        self.call(&[
            "api",
            "--method",
            "PATCH",
            &path,
            "-f",
            &format!("body={body}"),
        ])
        .map(drop)
    }

    fn close_issue(&self, repo: &Repo, number: u64, comment: &str) -> Result<(), PortError> {
        let path = format!("repos/{repo}/issues/{number}");
        let comments = format!("{path}/comments");
        self.call(&[
            "api",
            "--method",
            "POST",
            &comments,
            "-f",
            &format!("body={comment}"),
        ])?;
        self.call(&[
            "api",
            "--method",
            "PATCH",
            &path,
            "-f",
            "state=closed",
            "-f",
            "state_reason=completed",
        ])
        .map(drop)
    }
}

impl<E: Exec> ReleaseStore for Gh<E> {
    fn releases(&self, repo: &Repo) -> Result<Vec<Release>, PortError> {
        let (value, what) = self.api(&format!("repos/{repo}/releases?per_page=100"))?;
        let r = Reader { what: &what };
        r.list(&value, "the reply")?
            .iter()
            .map(|release| parse_release(&r, release))
            .collect()
    }

    fn create_draft(
        &self,
        repo: &Repo,
        tag: &str,
        commit: &Commit,
        notes: &str,
    ) -> Result<(), PortError> {
        self.call(&[
            "release",
            "create",
            tag,
            "--repo",
            repo.as_str(),
            "--target",
            commit.as_str(),
            "--title",
            tag,
            "--notes",
            notes,
            "--draft",
        ])
        .map(drop)
    }

    fn upload(&self, repo: &Repo, tag: &str, file: &Path) -> Result<(), PortError> {
        let file = utf8_path("gh release upload", file)?;
        self.call(&["release", "upload", tag, file, "--repo", repo.as_str()])
            .map(drop)
    }

    fn publish(&self, repo: &Repo, tag: &str) -> Result<(), PortError> {
        self.call(&[
            "release",
            "edit",
            tag,
            "--repo",
            repo.as_str(),
            "--draft=false",
        ])
        .map(drop)
    }
}

impl<E: Exec> Verifier for Gh<E> {
    /// Runs the commands of docs/deploy/verify-a-release.md: step 3 online, step 4 with the
    /// bundle (without its `--custom-trusted-root`: the runner fetches the trusted root), each
    /// with `--format json` so that the attestations it verified are read back and checked again.
    fn verify(
        &self,
        file: &Path,
        bundle: Option<&Path>,
        commit: &Commit,
    ) -> Result<Vec<Attestation>, PortError> {
        let mut args = vec![
            "attestation",
            "verify",
            utf8_path("gh attestation verify", file)?,
        ];
        if let Some(bundle) = bundle {
            args.push("--bundle");
            args.push(utf8_path("gh attestation verify", bundle)?);
        }
        args.extend([
            "--repo",
            REPO,
            "--signer-workflow",
            SIGNER_WORKFLOW,
            "--source-ref",
            SIGNER_REF,
            "--source-digest",
            commit.as_str(),
            "--deny-self-hosted-runners",
            "--format",
            "json",
        ]);
        let (value, what) = self.json(&args)?;
        let r = Reader { what: &what };
        r.list(&value, "the reply")?
            .iter()
            .map(|element| parse_attestation(&r, element))
            .collect()
    }
}

#[cfg(test)]
mod tests;
