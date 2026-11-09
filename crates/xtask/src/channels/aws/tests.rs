use super::*;
use crate::channels::fixtures::FakeExec;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

#[test]
fn aws_runs_the_program_it_was_given_with_the_arguments_and_returns_what_it_printed() {
    let exec = FakeExec::says("{\"ETag\": \"x\"}");
    let aws = ExecAws::new(&exec, PathBuf::from("/opt/aws"));
    let answer = aws.run(&args(&["s3api", "get-object"])).expect("answer");
    assert_eq!(
        answer,
        Answer {
            ok: true,
            stdout: "{\"ETag\": \"x\"}".to_owned(),
            stderr: String::new()
        }
    );
    assert_eq!(
        *exec.calls.borrow(),
        [("/opt/aws".to_owned(), args(&["s3api", "get-object"]))]
    );
}

#[test]
fn a_refusal_by_the_program_is_an_answer_and_a_program_that_does_not_start_is_an_error() {
    let refusing = FakeExec::fails("An error occurred (PreconditionFailed)");
    let answer = ExecAws::new(&refusing, PathBuf::from("aws"))
        .run(&args(&["x"]))
        .expect("answer");
    assert!(!answer.ok);
    assert_eq!(answer.stderr, "An error occurred (PreconditionFailed)");

    let missing = FakeExec::missing();
    let e = ExecAws::new(&missing, PathBuf::from("/nowhere/aws"))
        .run(&args(&["x"]))
        .unwrap_err();
    assert_eq!(
        e,
        Error::Spawn {
            what: "/nowhere/aws".to_owned(),
            why: "No such file or directory".to_owned()
        }
    );
}

#[test]
fn bytes_that_are_not_text_are_printed_with_replacement_characters() {
    let exec = FakeExec {
        calls: std::cell::RefCell::default(),
        answer: Ok(crate::release::ports::Ran {
            success: true,
            stdout: vec![b'a', 0xff],
            stderr: Vec::new(),
        }),
    };
    let answer = run_program(&exec, Path::new("aws"), &[]).expect("answer");
    assert_eq!(answer.stdout, "a\u{fffd}");
}

#[test]
fn a_bucket_is_lower_case_letters_digits_dashes_and_dots() {
    assert!(is_bucket("komira-apt-repo-123.example"));
    for bad in ["", "Bucket", "a_b", "a/b", "a b"] {
        assert!(!is_bucket(bad), "{bad:?}");
    }
}

#[test]
fn a_distribution_id_is_upper_case_letters_and_digits() {
    assert!(is_distribution_id("E1ABC23"));
    for bad in ["", "e1abc", "E1-A", "E 1"] {
        assert!(!is_distribution_id(bad), "{bad:?}");
    }
}
