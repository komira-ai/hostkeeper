//! `verify_each`, the batch of `gh attestation verify` commands `release check` asks for: every
//! ask answered in order, the first run alone and the rest as one batch, and a reply that is not
//! JSON refused.

use super::*;

#[test]
fn verify_each_answers_every_ask_in_order_and_a_path_it_cannot_name_runs_nothing() {
    // Catches: the answers of a batch matched to the wrong files (an ask refused before it ran
    // shifting the others), or a batch that stops at its first refusal.
    let first = verify_line("/x/a", None);
    let second = verify_line("/x/b", Some("/x/b.sigstore.json"));
    let first_words: Vec<&str> = first.iter().map(String::as_str).collect();
    let second_words: Vec<&str> = second.iter().map(String::as_str).collect();
    let api = Gh::new(
        FakeExec::default()
            .on(
                &first_words,
                true,
                &json!([element(SIGNER_URI)]).to_string(),
                "",
            )
            .on(&second_words, false, "", "Error: verification failed\n"),
    );
    let signer = Signer::hostkeeper();
    let pinned = commit();
    let bad = Path::new(OsStr::from_bytes(b"/x/a\xffb"));
    let ask = |file: &'static Path, bundle: Option<&'static Path>| Ask {
        signer: &signer,
        file,
        bundle,
        commit: &pinned,
    };
    let asks = [
        ask(Path::new("/x/a"), None),
        ask(bad, None),
        ask(Path::new("/x/b"), Some(Path::new("/x/b.sigstore.json"))),
    ];
    assert_eq!(
        api.verify_each(&asks),
        vec![
            Ok(vec![attestation()]),
            Err(failure(
                "gh attestation verify",
                "the path /x/a\u{fffd}b is not UTF-8"
            )),
            Err(failure(&second.join(" "), "Error: verification failed")),
        ]
    );
    assert_eq!(calls(&api), vec![first, second]);
    assert_eq!(api.verify_each(&[]), Vec::new());
}

/// A runner that records the size of each batch it is handed and fails every run.
#[derive(Default)]
struct Batches(std::cell::RefCell<Vec<usize>>);

impl Exec for Batches {
    fn spawn(
        &self,
        _program: &str,
        _args: &[String],
    ) -> std::io::Result<crate::release::ports::Ran> {
        Err(std::io::Error::other("not started"))
    }

    fn spawn_each(
        &self,
        runs: &[(String, Vec<String>)],
    ) -> Vec<std::io::Result<crate::release::ports::Ran>> {
        self.0.borrow_mut().push(runs.len());
        runs.iter().map(|(p, a)| self.spawn(p, a)).collect()
    }
}

#[test]
fn verify_each_runs_the_first_verification_alone_and_the_rest_as_one_batch() {
    // Catches: every verification started at once, so that each `gh` writes the trusted root's
    // cache at the same time as the others, or the batch split so that an ask is lost.
    let api = Gh::new(Batches::default());
    let signer = Signer::hostkeeper();
    let pinned = commit();
    let ask = Ask {
        signer: &signer,
        file: Path::new("/x/a"),
        bundle: None,
        commit: &pinned,
    };
    let answers = api.verify_each(&[ask; 4]);
    assert_eq!(answers.len(), 4);
    assert!(answers.iter().all(Result::is_err));
    assert_eq!(*api.exec.0.borrow(), [1, 3]);
}

#[test]
fn verify_refuses_a_reply_that_is_not_json() {
    // Catches: a verification whose reply cannot be read taken for one with no attestation, or
    // its error naming another command.
    let line = verify_line("/x/asset", None);
    let api = scripted(&line, "Error: rate limited");
    assert_eq!(
        api.verify(
            &Signer::hostkeeper(),
            Path::new("/x/asset"),
            None,
            &commit()
        ),
        Err(failure(
            &line.join(" "),
            "not JSON: expected value at line 1 column 1"
        ))
    );
}
