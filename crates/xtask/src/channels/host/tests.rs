use super::*;
use crate::channels::fixtures::FakeExec;

#[test]
fn a_program_runs_through_the_port_with_its_arguments() {
    let exec = FakeExec::says("body");
    let answer = ExecHost(&exec)
        .run(
            Path::new("curl"),
            &["--fail".to_owned(), "https://repo.example/file".to_owned()],
        )
        .expect("answer");
    assert_eq!(answer.stdout, "body");
    assert_eq!(
        *exec.calls.borrow(),
        [(
            "curl".to_owned(),
            vec!["--fail".to_owned(), "https://repo.example/file".to_owned()]
        )]
    );
}

#[test]
fn a_refusal_is_an_answer_and_a_missing_program_is_an_error() {
    let refusing = FakeExec::fails("curl: (22) The requested URL returned error: 404");
    let answer = ExecHost(&refusing)
        .run(Path::new("curl"), &[])
        .expect("answer");
    assert!(!answer.ok);
    let missing = FakeExec::missing();
    let e = ExecHost(&missing)
        .run(Path::new("/nowhere/curl"), &[])
        .unwrap_err();
    assert!(e.to_string().starts_with("/nowhere/curl: "), "{e}");
}
