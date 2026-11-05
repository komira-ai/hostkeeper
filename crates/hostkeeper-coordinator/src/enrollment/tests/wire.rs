//! The calls the coordinator answers are the proto's: their paths and messages are those of
//! `hostkeeper.agent.v1.Coordinator` in the descriptors `hostkeeper-proto` compiles, which its
//! compatibility test holds to the committed baseline.

use hostkeeper_proto::FILE_DESCRIPTOR_SET;
use prost::Message;
use prost_types::FileDescriptorSet;

use crate::enrollment::Method;

/// Catches a method answered under a path the descriptors do not hold, or decoded and encoded
/// as messages other than the proto's: the wire would change without the proto's
/// compatibility test seeing it.
#[test]
fn enrollment_rpcs_keep_the_wire_additive() {
    let set = FileDescriptorSet::decode(FILE_DESCRIPTOR_SET).expect("the descriptors");
    let service = set
        .file
        .iter()
        .filter(|file| file.package() == "hostkeeper.agent.v1")
        .flat_map(|file| file.service.iter())
        .find(|service| service.name() == "Coordinator")
        .expect("the Coordinator service");
    let served: Vec<(String, String, String)> = Method::ALL
        .iter()
        .map(|method| {
            let name = method.path().rsplit('/').next().unwrap_or_default();
            let found = service
                .method
                .iter()
                .find(|one| one.name() == name)
                .expect("a method of the descriptors");
            (
                format!("/hostkeeper.agent.v1.Coordinator/{}", found.name()),
                found.input_type().to_owned(),
                found.output_type().to_owned(),
            )
        })
        .collect();
    let want = [
        ("Enroll", "EnrollRequest", "EnrollResponse"),
        ("Poll", "PollRequest", "PollResponse"),
        ("Renew", "RenewRequest", "RenewResponse"),
    ]
    .map(|(method, input, output)| {
        (
            format!("/hostkeeper.agent.v1.Coordinator/{method}"),
            format!(".hostkeeper.agent.v1.{input}"),
            format!(".hostkeeper.agent.v1.{output}"),
        )
    });
    assert_eq!(served, want);
    assert!(
        Method::ALL
            .iter()
            .all(|m| m.needs_certificate() == (*m == Method::Renew))
    );
}
