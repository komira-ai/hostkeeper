use super::*;
use crate::release::fakes::scratch;

#[test]
fn the_host_is_the_url_without_scheme_port_or_path() {
    // Catches: a ping of the URL, or of the host with its port or path, which `tailscale ping`
    // does not answer.
    for (channel, host) in [
        ("https://channel.example:30443/releases", "channel.example"),
        ("https://channel.example:30443", "channel.example"),
        ("https://channel.example/releases/", "channel.example"),
        ("https://channel.example/", "channel.example"),
        ("https://channel.example", "channel.example"),
        (
            "https://Channel-1.tailnet.example:1",
            "Channel-1.tailnet.example",
        ),
        ("https://channel.example:65535/a/b?c#d", "channel.example"),
        ("https://192.0.2.7:30443/r", "192.0.2.7"),
        ("https://localhost", "localhost"),
    ] {
        assert_eq!(host_of(channel), Ok(host), "{channel}");
    }
}

#[test]
fn a_channel_that_is_not_an_https_url_of_a_host_is_refused_with_its_reason() {
    // Catches: an empty or malformed HOSTKEEPER_RELEASE_CHANNEL that would join and ping
    // nothing, or a wrong host, and leave the wait to fail half an hour later; a user name or
    // a bracketed address taken for the host; and a multi-line value passed on as a list.
    let scheme = "it does not start with https://".to_owned();
    let space = "it holds a space or a control character".to_owned();
    let no_host = "it names no host".to_owned();
    let port = |p: &str| format!("its port `{p}` is not a number from 1 to 65535");
    let host = |h: &str| {
        format!(
            "its host `{h}` is not a name or IPv4 address (letters, digits and `-`, in labels separated by single dots)"
        )
    };
    for (channel, why) in [
        ("", scheme.clone()),
        ("channel.example", scheme.clone()),
        ("http://channel.example:30443", scheme.clone()),
        ("HTTPS://channel.example", scheme),
        (" https://channel.example", space.clone()),
        (
            "https://channel.example\nhttps://other.example",
            space.clone(),
        ),
        ("https://channel.example\t", space),
        ("https://", no_host.clone()),
        ("https:///releases", no_host.clone()),
        ("https://:30443", no_host),
        ("https://channel.example:", port("")),
        ("https://channel.example:port", port("port")),
        ("https://channel.example:0", port("0")),
        ("https://channel.example:65536", port("65536")),
        ("https://channel.example:+1", port("+1")),
        (
            "https://[fd7a:115c:a1e0::1]:30443",
            host("[fd7a:115c:a1e0::1]"),
        ),
        ("https://user@channel.example", host("user@channel.example")),
        ("https://channel.example?x=1", host("channel.example?x=1")),
        ("https://channel.example#x", host("channel.example#x")),
        ("https://channel..example", host("channel..example")),
        ("https://.channel.example", host(".channel.example")),
        ("https://channel.example.", host("channel.example.")),
        ("https://chan_nel.example", host("chan_nel.example")),
    ] {
        assert_eq!(host_of(channel), Err(why), "{channel:?}");
    }
}

#[test]
fn the_verb_appends_the_hostname_line_to_its_output() {
    // Catches: the answer printed but not written to GITHUB_OUTPUT (the join then pings
    // nothing), or written under another key than the one the workflow reads.
    let root = scratch("channel-host");
    let output = root.join("output");
    std::fs::write(&output, "before=1\n").expect("write the output file");
    let lines = channel_host("https://channel.example:30443/releases", Some(&output))
        .expect("a channel with a host");
    assert_eq!(lines, ["hostname=channel.example"]);
    assert_eq!(
        std::fs::read_to_string(&output).expect("read the output file"),
        "before=1\nhostname=channel.example\n"
    );
    assert_eq!(
        channel_host("https://channel.example", None),
        Ok(vec!["hostname=channel.example".to_owned()])
    );
}

#[test]
fn a_refused_channel_writes_nothing_and_names_the_value_and_the_form() {
    // Catches: a partial answer written before the refusal, and a refusal that does not say
    // which value was wrong.
    let root = scratch("channel-host-refused");
    let output = root.join("output");
    assert_eq!(
        channel_host("http://channel.example", Some(&output)),
        Err(Error::Failed(
            "the release channel \"http://channel.example\" is not https://<host>[:<port>][/<path>]: it does not start with https://".to_owned()
        ))
    );
    assert!(!output.exists());
}

#[test]
fn the_verb_reads_its_flags_and_asks_for_no_adapters() {
    // Catches: `--channel` or `--output` not reaching the verb, the verb building the real
    // adapters though it reads no port, and `run_with` failing it instead of lending it back.
    use crate::release::ports::Adapters;
    use crate::release::tests::{connects, empty, text, verb};
    let root = scratch("channel-host-verb");
    let output = root.join("output");
    let argv = [
        "channel-host",
        "--channel",
        "https://channel.example:30443/releases",
        "--output",
        text(&output),
    ];
    let (out, connected) = connects(verb(&argv));
    assert_eq!(out, Ok(vec!["hostname=channel.example".to_owned()]));
    assert!(!connected);
    let fakes = empty();
    let refused = verb(&["channel-host", "--channel", ""]);
    assert!(matches!(
        crate::release::run_with(refused, &fakes.ports()),
        Err(Error::Failed(_))
    ));
    assert_eq!(
        std::fs::read_to_string(&output).expect("read the output file"),
        "hostname=channel.example\n"
    );
    assert!(crate::release::tests::parse(&["channel-host"]).is_err());
}
