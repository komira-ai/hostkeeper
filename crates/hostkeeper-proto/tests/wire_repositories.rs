//! The names on the wire of the coordinator's package repositories, pinned: the `Repositories`
//! service's RPCs, its refusal tokens, the new `ReleasesRefusal` token, and the fields of the new
//! messages by name, number and presence. The rest of the pinned names are in `wire.rs`.

use prost_reflect::DescriptorPool;

/// The tokens of an enum: each value's name without the enum's prefix, in lower case.
fn tokens(pool: &DescriptorPool, name: &str, prefix: &str) -> Vec<(String, i32)> {
    pool.get_enum_by_name(name)
        .expect("the enum")
        .values()
        .map(|v| {
            let token = v
                .name()
                .strip_prefix(prefix)
                .expect("the prefix")
                .to_ascii_lowercase();
            (token, v.number())
        })
        .collect()
}

/// Each field of a message, as its name, number and whether it tracks presence (`optional`),
/// in the file's order.
fn fields_of(pool: &DescriptorPool, message: &str) -> Vec<(String, u32, bool)> {
    pool.get_message_by_name(message)
        .expect("the message")
        .fields()
        .map(|f| (f.name().to_owned(), f.number(), f.supports_presence()))
        .collect()
}

/// A field's name, number and presence: `true` is a field whose absence differs from its zero.
type Field = (&'static str, u32, bool);

/// The fields of the new messages, pinned by name, number and presence.
const FIELDS: [(&str, &[Field]); 14] = [
    (
        "hostkeeper.release.v1.Repository",
        &[
            ("name", 1, false),
            ("kind", 2, false),
            ("uri", 3, false),
            ("suites", 4, false),
            ("components", 5, false),
            ("fingerprints", 6, false),
            ("keyring", 7, false),
        ],
    ),
    (
        "hostkeeper.agent.v1.RepositoriesReport",
        &[
            ("generation", 1, false),
            ("digest", 2, false),
            ("refused", 3, true),
        ],
    ),
    (
        "hostkeeper.agent.v1.RepositoryRefusal",
        &[
            ("generation", 1, false),
            ("digest", 2, false),
            ("reason", 3, false),
        ],
    ),
    (
        "hostkeeper.agent.v1.RepositorySet",
        &[
            ("generation", 1, false),
            ("digest", 2, false),
            ("entries", 3, false),
        ],
    ),
    (
        "hostkeeper.operator.v1.HostRepositories",
        &[
            ("named", 1, false),
            ("unregistered", 2, false),
            ("digest", 3, false),
            ("generation", 4, false),
            ("current", 5, false),
            ("refused", 6, false),
        ],
    ),
    (
        "hostkeeper.operator.v1.RegisteredRepository",
        &[
            ("repository", 1, true),
            ("keyring_sha256", 2, false),
            ("generation", 3, false),
            ("services", 4, false),
        ],
    ),
    ("hostkeeper.operator.v1.ListRepositoriesRequest", &[]),
    (
        "hostkeeper.operator.v1.ListRepositoriesResponse",
        &[
            ("repositories", 1, false),
            ("generation", 2, false),
            ("unregistered", 3, false),
        ],
    ),
    (
        "hostkeeper.operator.v1.GetRepositoryRequest",
        &[("name", 1, false)],
    ),
    (
        "hostkeeper.operator.v1.GetRepositoryResponse",
        &[("repository", 1, true)],
    ),
    (
        "hostkeeper.operator.v1.PutRepositoryRequest",
        &[("repository", 1, true), ("reason", 2, false)],
    ),
    (
        "hostkeeper.operator.v1.PutRepositoryResponse",
        &[("repository", 1, true), ("entry", 2, true)],
    ),
    (
        "hostkeeper.operator.v1.RemoveRepositoryRequest",
        &[("name", 1, false), ("reason", 2, false)],
    ),
    (
        "hostkeeper.operator.v1.RemoveRepositoryResponse",
        &[("generation", 1, false), ("entry", 2, true)],
    ),
];

/// One field of an existing message, by message, name, number and presence.
const ADDED: [(&str, &str, u32, bool); 4] = [
    (
        "hostkeeper.agent.v1.HeartbeatRequest",
        "repositories",
        11,
        true,
    ),
    (
        "hostkeeper.agent.v1.HeartbeatResponse",
        "repositories",
        5,
        true,
    ),
    (
        "hostkeeper.operator.v1.ServiceEntry",
        "repository",
        10,
        false,
    ),
    ("hostkeeper.operator.v1.Host", "repositories", 24, true),
];

/// Catches: a name of the repositories API renamed or renumbered without this test changing with
/// it: the `Repositories` RPCs with their messages, the refusal tokens and their numbers, the
/// `SetDesired` token `repository_unknown`, the fields of each new message with the ones that can
/// be absent, and the fields added to the heartbeat, the host and the service entry. Callers
/// branch on each, and the operator client, the coordinator and the daemon are built against
/// them apart.
#[test]
fn wire_names_of_the_repositories_api_are_pinned() {
    let pool = DescriptorPool::decode(hostkeeper_proto::FILE_DESCRIPTOR_SET).expect("decodes");

    let service = pool
        .get_service_by_name("hostkeeper.operator.v1.Repositories")
        .expect("the service");
    let rpcs: Vec<String> = service
        .methods()
        .map(|m| format!("{}({}) {}", m.name(), m.input().name(), m.output().name()))
        .collect();
    assert_eq!(
        rpcs,
        [
            "List(ListRepositoriesRequest) ListRepositoriesResponse",
            "Get(GetRepositoryRequest) GetRepositoryResponse",
            "Put(PutRepositoryRequest) PutRepositoryResponse",
            "Remove(RemoveRepositoryRequest) RemoveRepositoryResponse",
        ]
    );

    let owned = |pairs: &[(&str, i32)]| -> Vec<(String, i32)> {
        pairs.iter().map(|(n, k)| ((*n).to_owned(), *k)).collect()
    };
    assert_eq!(
        tokens(
            &pool,
            "hostkeeper.operator.v1.RepositoriesRefusal",
            "REPOSITORIES_REFUSAL_"
        ),
        owned(&[
            ("unspecified", 0),
            ("unauthorized", 1),
            ("forbidden", 2),
            ("invalid_request", 3),
            ("keyring_invalid", 4),
            ("unknown_repository", 5),
            ("in_use", 6),
            ("conflict", 7),
            ("capacity", 8),
            ("store_failed", 9),
            ("audit_unavailable", 10),
        ])
    );
    let releases = tokens(
        &pool,
        "hostkeeper.operator.v1.ReleasesRefusal",
        "RELEASES_REFUSAL_",
    );
    assert!(releases.contains(&("repository_unknown".to_owned(), 28)));

    for (message, want) in FIELDS {
        let want: Vec<(String, u32, bool)> = want
            .iter()
            .map(|(name, number, presence)| ((*name).to_owned(), *number, *presence))
            .collect();
        assert_eq!(fields_of(&pool, message), want, "{message}");
    }
    for (message, field, number, presence) in ADDED {
        let found = fields_of(&pool, message)
            .into_iter()
            .find(|(name, _, _)| name == field)
            .unwrap_or_else(|| panic!("{message}.{field}"));
        assert_eq!(found, (field.to_owned(), number, presence), "{message}");
    }
}
