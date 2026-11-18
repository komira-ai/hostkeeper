//! The tests of `identity_group`: the group file in a scratch directory, `dscl` and the tools that
//! make a group as scripted programs. No real group file or directory service is read and no
//! group is made.

use std::sync::{Arc, Mutex};

use hostkeeper_launchd::{FakeLaunchctl, LaunchctlError, LaunchctlOutput};
use hostkeeper_testkit::Scratch;

use super::*;

pub(crate) fn scratch(name: &str) -> Scratch {
    let exe = std::env::current_exe().expect("the test binary");
    let target = exe.ancestors().nth(3).expect("the target directory");
    Scratch::under(&target.join("tmp"), name).expect("a scratch directory")
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| (*word).to_owned()).collect()
}

/// A tool that makes groups: it answers `answer` and, when `writes` is a line, appends it to the
/// group file `file`, as a tool that made the group would; it keeps the arguments of each call.
#[derive(Clone)]
pub(crate) struct MakesGroup {
    file: PathBuf,
    writes: Option<String>,
    answer: Result<LaunchctlOutput, LaunchctlError>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl MakesGroup {
    /// A tool that exits 0 and appends `line` to `file`.
    pub(crate) fn making(file: &Path, line: &str) -> MakesGroup {
        MakesGroup {
            file: file.to_owned(),
            writes: Some(line.to_owned()),
            answer: Ok(LaunchctlOutput::success("")),
            calls: Arc::default(),
        }
    }

    /// A tool that answers `answer` and writes nothing.
    pub(crate) fn answering(answer: Result<LaunchctlOutput, LaunchctlError>) -> MakesGroup {
        MakesGroup {
            file: PathBuf::new(),
            writes: None,
            answer,
            calls: Arc::default(),
        }
    }

    pub(crate) fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("the calls").clone()
    }
}

impl Launchctl for MakesGroup {
    fn run(&self, args: &[String]) -> Result<LaunchctlOutput, LaunchctlError> {
        self.calls.lock().expect("the calls").push(args.to_vec());
        if let Some(line) = &self.writes {
            let mut text = fs::read_to_string(&self.file).unwrap_or_default();
            text.push_str(line);
            text.push('\n');
            fs::write(&self.file, text).expect("the group file");
        }
        self.answer.clone()
    }
}

fn group_file(scratch: &Scratch, text: &str) -> PathBuf {
    let path = scratch.path().join("group");
    fs::write(&path, text).expect("the group file");
    path
}

const ETC: &str =
    "root:x:0:\nwheel:x:10:alice\nhostkeeper-identity2:x:901:\nhostkeeper-identity:x:977:svc\n";

fn etc_group(file: &Path, tool: &MakesGroup) -> IdentityGroup {
    IdentityGroup::in_group_file(
        IDENTITY_GROUP,
        file.to_owned(),
        Box::new(tool.clone()),
        PlatformKind::Linux,
    )
}

/// Proves each platform makes the group with its own tool and arguments: `groupadd --system` under
/// systemd, busybox `addgroup -S` under OpenRC, `dseditgroup -o create` on a Mac.
#[test]
fn each_platform_makes_the_group_with_its_own_tool() {
    assert_eq!(
        make_command(PlatformKind::Linux, "g"),
        ("/usr/sbin/groupadd", words(&["--system", "g"]))
    );
    assert_eq!(
        make_command(PlatformKind::Openrc, "g"),
        ("/usr/sbin/addgroup", words(&["-S", "g"]))
    );
    assert_eq!(
        make_command(PlatformKind::Macos, "g"),
        ("/usr/sbin/dseditgroup", words(&["-o", "create", "g"]))
    );
}

/// Proves the host's group is the one named `hostkeeper-identity`, looked up in `/etc/group` on a
/// Linux host (either init system) and through `dscl` on a Mac.
#[test]
fn the_host_group_is_looked_up_where_its_platform_keeps_groups() {
    for kind in [PlatformKind::Linux, PlatformKind::Openrc] {
        let group = IdentityGroup::for_host(kind);
        assert_eq!(group.name, "hostkeeper-identity");
        assert!(
            matches!(&group.directory, Directory::Etc(file) if file == Path::new("/etc/group"))
        );
    }
    let group = IdentityGroup::for_host(PlatformKind::Macos);
    assert_eq!(group.name, "hostkeeper-identity");
    assert!(matches!(group.directory, Directory::Dscl(_)));
}

/// Proves the gid is the third field of the line whose name is the group's, not of a line whose
/// name only starts with it, and that a group that is there is not made again.
#[test]
fn a_group_in_the_group_file_gives_its_gid_and_is_not_made_again() {
    let scratch = scratch("ig-found");
    let file = group_file(&scratch, ETC);
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    assert_eq!(etc_group(&file, &tool).ensure().expect("a gid"), 977);
    assert_eq!(tool.calls(), Vec::<Vec<String>>::new());
}

/// Proves a group the host lacks is made once, by the tool with its arguments, and its gid is the
/// one the group file then gives.
#[test]
fn a_missing_group_is_made_once_and_its_gid_is_then_found() {
    let scratch = scratch("ig-make");
    let file = group_file(&scratch, "root:x:0:\n");
    let tool = MakesGroup::making(&file, "hostkeeper-identity:x:988:");
    let mut group = etc_group(&file, &tool);
    assert_eq!(group.ensure().expect("a gid"), 988);
    assert_eq!(tool.calls(), vec![words(&["--system", IDENTITY_GROUP])]);
    assert_eq!(group.ensure().expect("a gid"), 988);
    assert_eq!(
        tool.calls().len(),
        1,
        "a group that is there is not made again"
    );
}

/// Proves a tool that exits with a code is reported with the step and the first line of what it
/// wrote, and that the group is not looked up as made.
#[test]
fn a_tool_that_fails_says_its_exit_code_and_first_stderr_line() {
    let scratch = scratch("ig-fail");
    let file = group_file(&scratch, "root:x:0:\n");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::exit(9, "groupadd: boom\nmore\n")));
    let error = etc_group(&file, &tool).ensure().expect_err("a failure");
    assert_eq!(
        error.to_string(),
        "making the identity group failed with exit code 9: groupadd: boom"
    );
}

/// Proves a tool that cannot be started is reported as one that could not be run.
#[test]
fn a_tool_that_cannot_be_started_says_so() {
    let scratch = scratch("ig-spawn");
    let file = group_file(&scratch, "root:x:0:\n");
    let tool = MakesGroup::answering(Err(LaunchctlError::Spawn {
        args: "x".to_owned(),
        why: "no such file".to_owned(),
    }));
    let error = etc_group(&file, &tool).ensure().expect_err("a failure");
    assert_eq!(
        error.to_string(),
        "making the identity group could not be run: no such file"
    );
}

/// Proves a tool that exited 0 and made nothing is an error, not a gid.
#[test]
fn a_tool_that_makes_nothing_leaves_the_group_missing() {
    let scratch = scratch("ig-nothing");
    let file = group_file(&scratch, "root:x:0:\n");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let error = etc_group(&file, &tool).ensure().expect_err("a failure");
    assert!(matches!(&error, GroupError::StillMissing(name) if name == IDENTITY_GROUP));
    assert_eq!(
        error.to_string(),
        "the group hostkeeper-identity is not there after it was made"
    );
}

/// Proves a group file that cannot be read is an error naming the file, and that nothing is made.
#[test]
fn a_group_file_that_cannot_be_read_is_an_error_and_makes_nothing() {
    let scratch = scratch("ig-unreadable");
    let file = scratch.path().join("absent");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let error = etc_group(&file, &tool).ensure().expect_err("a failure");
    assert_eq!(
        error.to_string(),
        format!(
            "cannot read the group file {}: No such file or directory (os error 2)",
            file.display()
        )
    );
    assert!(tool.calls().is_empty());
}

/// Proves a line of the group that gives no number as its gid, or no third field, is an error
/// naming the group and the file, not a group to make.
#[test]
fn a_group_line_without_a_gid_is_an_error() {
    let scratch = scratch("ig-malformed");
    for text in [
        "hostkeeper-identity:x:abc:\n",
        "hostkeeper-identity:x\n",
        "hostkeeper-identity\n",
    ] {
        let file = group_file(&scratch, text);
        let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
        let error = etc_group(&file, &tool).ensure().expect_err("a failure");
        assert_eq!(
            error.to_string(),
            format!(
                "the line of the group hostkeeper-identity in {} does not give a gid",
                file.display()
            ),
            "{text:?}"
        );
        assert!(tool.calls().is_empty());
    }
}

const LIST: &[&str] = &[".", "-list", "/Groups", "PrimaryGroupID"];

fn mac(dscl: &FakeLaunchctl, maker: &MakesGroup) -> IdentityGroup {
    IdentityGroup::in_directory_service(
        IDENTITY_GROUP,
        Box::new(dscl.clone()),
        Box::new(maker.clone()),
    )
}

/// Proves a Mac's group is found in the list `dscl` writes by name, not by a longer name, and is
/// not made when it is there.
#[test]
fn a_mac_group_in_the_directory_service_gives_its_gid() {
    let dscl = FakeLaunchctl::new();
    dscl.always(
        &words(LIST),
        Ok(LaunchctlOutput::success(
            "admin  80\nhostkeeper-identity2  600\nhostkeeper-identity   601\n",
        )),
    );
    let maker = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    assert_eq!(mac(&dscl, &maker).ensure().expect("a gid"), 601);
    assert!(maker.calls().is_empty());
}

/// Proves a Mac that lacks the group makes it with `dseditgroup -o create` once and then reads its
/// gid from the directory service.
#[test]
fn a_mac_makes_a_missing_group_with_dseditgroup_and_reads_its_gid() {
    let dscl = FakeLaunchctl::new();
    dscl.then(&words(LIST), Ok(LaunchctlOutput::success("admin  80\n")));
    dscl.then(
        &words(LIST),
        Ok(LaunchctlOutput::success(
            "admin  80\nhostkeeper-identity  402\n",
        )),
    );
    let maker = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    assert_eq!(mac(&dscl, &maker).ensure().expect("a gid"), 402);
    assert_eq!(
        maker.calls(),
        vec![words(&["-o", "create", IDENTITY_GROUP])]
    );
}

/// Proves a `dscl` that fails, and a group line with no number, are errors and make nothing.
#[test]
fn a_mac_whose_directory_service_fails_or_gives_no_gid_makes_nothing() {
    let maker = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let dscl = FakeLaunchctl::new();
    dscl.always(&words(LIST), Ok(LaunchctlOutput::exit(56, "")));
    assert_eq!(
        mac(&dscl, &maker)
            .ensure()
            .expect_err("a failure")
            .to_string(),
        "dscl list of groups failed with exit code 56"
    );
    let dscl = FakeLaunchctl::new();
    dscl.always(
        &words(LIST),
        Ok(LaunchctlOutput::success("hostkeeper-identity\n")),
    );
    assert_eq!(
        mac(&dscl, &maker)
            .ensure()
            .expect_err("a failure")
            .to_string(),
        "the line of the group hostkeeper-identity in the directory service does not give a gid"
    );
    assert!(maker.calls().is_empty());
}

/// Proves a group file that cannot be read keeps the operating system's error as the source of the
/// error, for a reader that walks the chain.
#[test]
fn a_group_file_that_cannot_be_read_carries_the_io_error_as_its_source() {
    let scratch = scratch("ig-source");
    let file = scratch.path().join("absent");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let error = etc_group(&file, &tool).ensure().expect_err("a failure");
    let source = std::error::Error::source(&error).expect("a source");
    assert_eq!(source.to_string(), "No such file or directory (os error 2)");
}

/// Proves a tool that failed is run again once every `LOOKUP_EVERY` steps and not at the steps
/// between, which look the group up as always and give the failure again, and that the group is
/// found at once, without the tool, when it appears between two tries.
///
/// Catches: a group tool that runs at every 5 second step of a host where it cannot succeed.
#[test]
fn a_failing_group_tool_is_run_once_every_twelve_steps() {
    let scratch = scratch("ig-backoff");
    let file = group_file(&scratch, "root:x:0:\n");
    let tool = MakesGroup::answering(Ok(LaunchctlOutput::exit(9, "groupadd: boom")));
    let mut group = etc_group(&file, &tool);
    let boom = "making the identity group failed with exit code 9: groupadd: boom";
    for step in 1..=36 {
        let error = group.ensure().expect_err("a failure");
        assert_eq!(error.to_string(), boom, "step {step}");
        let tries = [1, 13, 25].iter().filter(|&&at| at <= step).count();
        assert_eq!(tool.calls().len(), tries, "step {step}");
    }
    assert_eq!(group.ensure().expect_err("a failure").to_string(), boom);
    assert_eq!(tool.calls().len(), 4, "the step after 36 is the 37th");
    fs::write(&file, ETC).expect("the group appears");
    assert_eq!(group.ensure().expect("a gid"), 977);
    assert_eq!(tool.calls().len(), 4);
}

/// Proves a Mac runs `dscl` once every `LOOKUP_EVERY` steps while the gid it gave is sound, uses
/// the gid kept in between, and looks again at once after `forget`.
#[test]
fn a_mac_runs_dscl_once_every_twelve_steps_while_its_gid_is_kept() {
    let dscl = FakeLaunchctl::new();
    dscl.always(
        &words(LIST),
        Ok(LaunchctlOutput::success("hostkeeper-identity  601\n")),
    );
    let maker = MakesGroup::answering(Ok(LaunchctlOutput::success("")));
    let mut group = mac(&dscl, &maker);
    for step in 1..=36 {
        assert_eq!(group.ensure().expect("a gid"), 601, "step {step}");
        let runs = [1, 13, 25].iter().filter(|&&at| at <= step).count();
        assert_eq!(dscl.calls().len(), runs, "step {step}");
    }
    group.forget();
    assert_eq!(group.ensure().expect("a gid"), 601);
    assert_eq!(dscl.calls().len(), 4);
    assert!(maker.calls().is_empty());
}
