//! `--kernel-parameter`: the words accepted at the bounds, every refusal by its exact message, a
//! run's parameters checked as a whole (count, a name given twice), and the GRUB defaults line
//! they make.

use hostkeeper_pxe_seed::kernel::{
    KERNEL_PARAMETERS_FILE, KernelParameter, KernelParameterError, KernelParameters,
    KernelParametersError, MAX_KERNEL_PARAMETERS,
};

fn word(text: &str) -> KernelParameter {
    text.parse().expect(text)
}

/// Catches a name cut at the wrong `=`, a value with an `=` refused, or a word changed on the way
/// in.
#[test]
fn a_word_and_its_name_are_read() {
    for (text, name) in [
        ("fuse.enable_uring=1", "fuse.enable_uring"),
        ("quiet", "quiet"),
        ("a-b_c.D9=x=y", "a-b_c.D9"),
        ("foo=", "foo"),
        ("rd.lvm.vg=x,y:z/@#%^&*()[]{}<>|;~!?+'", "rd.lvm.vg"),
    ] {
        let parameter = word(text);
        assert_eq!((parameter.as_str(), parameter.name()), (text, name));
    }
    let longest = format!("n={}", "v".repeat(254));
    assert_eq!(word(&longest).as_str(), longest);
}

/// Every refusal, by its exact message: an empty word, one over 256 bytes, an empty name, a name
/// with a character outside `A-Za-z0-9._-`, and a word holding a space, a tab, a quote, a
/// backslash, a `$`, a backtick or a byte outside printable ASCII.
///
/// Catches a word that would end the double-quoted `GRUB_CMDLINE_LINUX` line, expand in the shell
/// GRUB's scripts read it with, or split into two kernel arguments.
#[test]
fn each_word_the_command_line_cannot_carry_is_refused() {
    let too_long = format!("n={}", "v".repeat(255));
    for text in [
        "",
        too_long.as_str(),
        "=1",
        "a/b=1",
        "a:b",
        "na\u{e9}me",
        "a b",
        "a\tb",
        "a=\"x\"",
        "a=x\\y",
        "a=$x",
        "a=`x`",
        "a=\u{e9}",
        "a=x\n",
    ] {
        let refused = text.parse::<KernelParameter>().expect_err(text);
        assert_eq!(refused, KernelParameterError, "{text:?}");
        assert_eq!(
            refused.to_string(),
            "--kernel-parameter is not <name> or <name>=<value>: 1 to 256 printable ASCII characters without a space, a quote, a backslash, a $ or a backtick, and a name of A-Za-z0-9._-",
            "{text:?}"
        );
    }
}

fn many(count: usize) -> Vec<KernelParameter> {
    (0..count).map(|n| word(&format!("p{n}=1"))).collect()
}

/// A run's parameters keep their order in the GRUB defaults line; none and up to the limit are
/// accepted; over the limit, and one name given twice, with or without a value, are refused
/// with their exact messages.
///
/// Catches words reordered, joined without a space, a line that replaces `GRUB_CMDLINE_LINUX`
/// instead of adding to it, a limit one off, and two values of one name, of which the kernel
/// would take one by an order nobody chose.
#[test]
fn a_runs_parameters_are_checked_as_a_whole_and_make_one_line() {
    assert_eq!(
        KernelParameters::new(Vec::new()),
        Ok(KernelParameters::default())
    );
    assert!(KernelParameters::default().parameters().is_empty());
    let three = KernelParameters::new(vec![
        word("fuse.enable_uring=1"),
        word("quiet"),
        word("mitigations=off"),
    ])
    .expect("three names");
    assert_eq!(
        three.grub_defaults(),
        "GRUB_CMDLINE_LINUX=\"$GRUB_CMDLINE_LINUX fuse.enable_uring=1 quiet mitigations=off\"\n"
    );
    let words: Vec<&str> = three
        .parameters()
        .iter()
        .map(KernelParameter::as_str)
        .collect();
    assert_eq!(words, ["fuse.enable_uring=1", "quiet", "mitigations=off"]);
    let most = KernelParameters::new(many(MAX_KERNEL_PARAMETERS)).expect("the most");
    assert_eq!(most.parameters().len(), 32);
    let refused = KernelParameters::new(many(MAX_KERNEL_PARAMETERS + 1)).expect_err("too many");
    assert_eq!(refused, KernelParametersError::TooMany(33));
    assert_eq!(
        refused.to_string(),
        "--kernel-parameter is given 33 times, over 32"
    );
    for pair in [["a=1", "a=2"], ["a", "a=1"], ["a=1", "a"], ["a", "a"]] {
        let refused = KernelParameters::new(vec![word("b"), word(pair[0]), word(pair[1])])
            .expect_err("one name twice");
        assert_eq!(
            refused.to_string(),
            "--kernel-parameter's name a is given twice",
            "{pair:?}"
        );
    }
    assert_eq!(
        KERNEL_PARAMETERS_FILE,
        "/etc/default/grub.d/98_hostkeeper_parameters.cfg"
    );
}
