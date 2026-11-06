//! hello's release and its failing build's, staged into one channel tree as the release assets
//! arrive (`hello-index.json` and `hello-fail-ready-index.json` side by side): each at its own
//! version, so a host ordered either fetches that build.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{
    ARCHIVE, VERSION, bundle_name, bytes_of, dirs, fetched, lines, package_name, request_of, tree,
    write_attested_release,
};
use crate::release::channel_stage::{Error, Platform, Request, stage};

#[test]
fn hello_and_its_failing_build_are_staged_at_their_own_versions_side_by_side() {
    // Catches: the failing build staged at hello's own version (one overwriting or refused against
    // the other, so a host ordered `<V>-fail-ready` fetches the good build or nothing), its index
    // naming the plain version, or either index accepted for the other's version.
    let d = dirs("channel-stage-fail-ready");
    let failing = format!("{VERSION}-fail-ready");
    for (v, index) in [
        (VERSION, "hello-index.json"),
        (&*failing, "hello-fail-ready-index.json"),
    ] {
        write_attested_release(&d.from, "hello", v);
        std::fs::rename(d.from.join("index.json"), d.from.join(index)).expect("rename the index");
    }
    let staged = |v: &str, index: &str| {
        stage(&Request {
            index: Some(d.from.join(index)),
            ..request_of(&d, "hello", v)
        })
    };
    assert_eq!(
        staged(VERSION, "hello-index.json"),
        Ok(lines(&d.out, "hello", VERSION, &Platform::ALL, "staged"))
    );
    assert_eq!(
        staged(&failing, "hello-fail-ready-index.json"),
        Ok(lines(&d.out, "hello", &failing, &Platform::ALL, "staged"))
    );
    let mut want = BTreeMap::new();
    for (v, index) in [
        (VERSION, "hello-index.json"),
        (&*failing, "hello-fail-ready-index.json"),
    ] {
        let read = |name: &str| std::fs::read(d.from.join(name)).expect("read an asset");
        for platform in Platform::ALL {
            for name in [
                package_name("hello", v, platform),
                bundle_name("hello", v, platform),
            ] {
                want.insert(format!("hello/{v}/{name}"), read(&name));
            }
        }
        want.insert(format!("hello/{v}/index.json"), read(index));
    }
    assert_eq!(bytes_of(&tree(&d.out)), want);

    let index_version = |v: &str| {
        let text = std::fs::read_to_string(d.out.join("hello").join(v).join("index.json"))
            .expect("read a staged index");
        let index: Value = serde_json::from_str(&text).expect("JSON");
        index["version"].as_str().expect("a version").to_owned()
    };
    assert_eq!(index_version(VERSION), "1.5.0");
    assert_eq!(index_version(&failing), "1.5.0-fail-ready");

    for v in [VERSION, &*failing] {
        let (bytes, asked) = fetched(&d.out, "hello", v, Platform::LinuxX86_64);
        assert_eq!(bytes, ARCHIVE, "{v}");
        assert_eq!(asked[0], format!("hello/{v}/index.json"));
    }

    for (v, index) in [
        (VERSION, "hello-fail-ready-index.json"),
        (&*failing, "hello-index.json"),
    ] {
        let Err(Error::Failed(why)) = staged(v, index) else {
            panic!("the index of the other build is staged as {v}'s");
        };
        assert!(why.contains("index_invalid"), "{why}");
    }
}
