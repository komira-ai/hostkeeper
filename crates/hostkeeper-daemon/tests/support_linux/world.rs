//! The coordinator of a test: its enrollment, its groups and the operator's calls.

use super::*;

/// The operator, as the coordinator's in-process calls take it.
fn operator() -> Caller {
    Caller::Operator(hostkeeper_coordinator::tokens::Operator::named(
        "operator".parse().expect("a token name"),
    ))
}

/// The coordinator's enrollment over the files of `dir` (its store, its CA, made at the first
/// start, and its server certificate), as the coordinator's own start makes it from its flags:
/// a coordinator is built from the flags of `serve` (its other files in `dir/start`, listening
/// nowhere, no address dialled) and its enrollment kept; the rest of it is dropped.
fn enrollment_of(dir: &Path) -> Arc<Enrollment> {
    let start = dir.join("start");
    fs::create_dir_all(&start).expect("the directory of the start's files");
    let put = |name: &str, text: &str, mode: u32| -> std::ffi::OsString {
        let path = start.join(name);
        fs::write(&path, text).expect("written");
        fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(mode))
            .expect("its mode");
        path.into_os_string()
    };
    let listener = Ca::new("private listener CA").coordinator();
    let at = |name: &str| dir.join(name).into_os_string();
    let args: Vec<std::ffi::OsString> = vec![
        "hostkeeper-coordinator".into(),
        "serve".into(),
        // Documentation addresses: nothing is bound or dialled.
        "--heartbeat-listen".into(),
        "192.0.2.1:1".into(),
        "--private-listen".into(),
        "192.0.2.1:2".into(),
        "--tls-cert".into(),
        put("tls.pem", &listener.cert_pem, 0o644),
        "--tls-key".into(),
        put("tls.key", &listener.key_pem, 0o600),
        "--client-ca".into(),
        put("ca.pem", &Ca::new("farm CA").pem, 0o644),
        "--state-file".into(),
        start.join("desired.json").into_os_string(),
        "--oidc-config".into(),
        put("oidc.toml", &config_text(STATIC_ISSUER), 0o644),
        "--operator-token-file".into(),
        put("operator.token", OPERATOR, 0o600),
        "--legacy-service".into(),
        "kbf".into(),
        "--enrollment-dir".into(),
        at("enrollment"),
        "--ca-key-file".into(),
        at("ca.key"),
        "--ca-init".into(),
        "--server-cert-file".into(),
        at("server.pem"),
        "--server-name".into(),
        "coordinator.example".into(),
    ];
    let Command::Serve(flags) = Cli::try_parse_from(args).expect("the flags").command else {
        panic!("the verb is serve");
    };
    let roots = Roots::Given(vec![CertificateDer::from(Ca::new("issuer roots").der)]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let parts = {
        let _entered = runtime.enter();
        build(&flags, roots).expect("the coordinator starts")
    };
    Arc::clone(parts.coordinator.enrollment().expect("enrollment"))
}

/// The certificate of the updater `host` of the role `role`, as the coordinator reads the peer
/// of a heartbeat.
pub(super) fn peer(host: &str, role: &str) -> Peer {
    let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
    let uri = |text: String| SanType::URI(text.try_into().expect("a URI"));
    params.subject_alt_names = vec![
        uri(format!("{DEFAULT_ROLE_URI_PREFIX}{UPDATER_ROLE}")),
        uri(format!("{DEFAULT_HOST_ROLE_URI_PREFIX}{role}")),
        SanType::DnsName(host.to_owned().try_into().expect("a DNS name")),
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    let key = KeyPair::generate().expect("a key");
    let der = params.self_signed(&key).expect("signed").der().to_vec();
    Some(Arc::from(der.as_slice()))
}

/// The coordinator of a test, on the state files of a directory of its own, which a restart
/// reads again.
pub struct World {
    dir: Scratch,
    coordinator: Mutex<Option<Arc<Coordinator>>>,
}

impl World {
    /// A coordinator on a new directory named `name`, holding no group and no host.
    pub fn new(name: &str) -> Arc<World> {
        let dir = Scratch::new(name);
        let coordinator = Mutex::new(Some(Self::start(dir.path())));
        Arc::new(World { dir, coordinator })
    }

    /// The real coordinator on the state files of `dir`, with its enrollment over the files of
    /// `dir` and the operator's token [`OPERATOR`]: it orders from its start (no quiet period),
    /// on a clock that stays at second 1000, so every host it heard stays live. The ops store's
    /// clock, which a run's window and a deferral's backoff are read on, is the wall clock.
    fn start(dir: &Path) -> Arc<Coordinator> {
        let state = dir.join("desired.json");
        let verdicts = VerdictFile::open(&verdicts_path(&state)).expect("the verdicts file");
        let desired = DesiredStore::open(&state, None).expect("the desired state");
        let ops = OpsStore::open(&dir.join("desired.json.ops")).expect("the ops store");
        let allow = format!(
            "{}services = [\"hostkeeper\"]\n",
            config_text(STATIC_ISSUER)
        );
        let (oidc_now, _) = oidc_clock();
        let verifier = Verifier::new(
            OidcConfig::parse(&allow).expect("the allow-list"),
            Arc::new(StaticKeys(jwks_of(&[&keys()[0]]))),
            Duration::from_secs(30),
            oidc_now,
            spent(),
        );
        let enrollment = enrollment_of(dir);
        let clock: Clock = Arc::new(|| Duration::from_secs(1_000));
        Arc::new(
            Coordinator::new(desired, Arc::new(verifier), OPERATOR.to_owned(), clock)
                .with_quiet(Duration::ZERO)
                .with_ops(ops)
                .with_verdicts(verdicts)
                .with_enrollment(enrollment),
        )
    }

    /// The coordinator stops and a new one starts on the same files: what it stored is read
    /// again, and what it kept in memory (each host's last heartbeat) is gone.
    pub fn restart(&self) {
        let mut held = self.coordinator.lock().expect("not poisoned");
        // The old coordinator ends, and its files are closed, before the new one opens them.
        *held = None;
        *held = Some(Self::start(self.dir.path()));
    }

    /// The coordinator running now.
    pub fn coordinator(&self) -> Arc<Coordinator> {
        Arc::clone(
            self.coordinator
                .lock()
                .expect("not poisoned")
                .as_ref()
                .expect("a coordinator runs"),
        )
    }

    /// The operator's call of `path` with `request`: the answer, or the gRPC status code and
    /// message of a refusal.
    pub fn rpc<R: Message + Default>(
        &self,
        path: &str,
        request: &impl Message,
    ) -> Result<R, (i32, String)> {
        let (code, message, body) = call(
            &self.coordinator(),
            Surface::Control,
            &None,
            path,
            Some(OPERATOR),
            &request.encode_to_vec(),
        );
        if code != 0 {
            return Err((code, message));
        }
        Ok(R::decode(body.as_slice()).expect("an answer"))
    }

    /// The operator puts the group `name` (the package `hostkeeper` alone) with the maintenance
    /// table `maintenance`, then each of `hosts` enrolls and is approved into it.
    pub fn group(&self, name: &str, hosts: &[&str], maintenance: op::MaintenancePolicy) {
        self.coordinator()
            .host_groups_put(
                &operator(),
                op::PutHostGroupRequest {
                    host_group: Some(op::HostGroup {
                        name: name.to_owned(),
                        packages: vec!["hostkeeper".to_owned()],
                        policy: Some(op::RolloutPolicy {
                            maintenance: Some(maintenance),
                            ..op::RolloutPolicy::default()
                        }),
                    }),
                    if_generation: None,
                    reason: "the hosts of a maintenance test".to_owned(),
                },
            )
            .expect("the group is put");
        for host in hosts {
            self.enroll(host, name);
        }
    }

    /// `host` asks to enroll with a key of its own, and the operator approves it into `group`.
    fn enroll(&self, host: &str, group: &str) {
        let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("a key");
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "host:pending");
        let csr = params
            .serialize_request(&key)
            .expect("a CSR")
            .der()
            .to_vec();
        let coordinator = self.coordinator();
        let enrolled = coordinator
            .enrollment()
            .expect("enrollment")
            .enroll(pb::EnrollRequest {
                csr_der: csr,
                claims: Some(pb::Claims {
                    machine_id: format!("machine-{host}"),
                    ..pb::Claims::default()
                }),
                ..pb::EnrollRequest::default()
            })
            .expect("the request is kept");
        let _approved: op::ApproveResponse = self
            .rpc(
                APPROVE,
                &op::ApproveRequest {
                    request_id: enrolled.request_id.clone(),
                    key_sha256: enrolled.request_id,
                    host_id: (*host).to_owned(),
                    reason: "a host of a maintenance test".to_owned(),
                    host_group: group.to_owned(),
                    ..op::ApproveRequest::default()
                },
            )
            .expect("approved");
    }

    /// `Maintenance.Schedule` of `request`, confirmed, with a reason: the run's id.
    pub fn schedule(&self, request: op::ScheduleMaintenanceRequest) -> String {
        let answer: op::ScheduleMaintenanceResponse = self
            .rpc(
                SCHEDULE,
                &op::ScheduleMaintenanceRequest {
                    reason: "monthly updates".to_owned(),
                    confirm: true,
                    ..request
                },
            )
            .expect("scheduled");
        answer.run.expect("the run").id
    }

    /// The Update button on the hosts `hosts`: a `Schedule` of every kind due, now, with no batch
    /// and no drain deadline of its own.
    pub fn update(&self, hosts: &[&str]) -> String {
        self.schedule(op::ScheduleMaintenanceRequest {
            host_ids: hosts.iter().map(|host| (*host).to_owned()).collect(),
            ..op::ScheduleMaintenanceRequest::default()
        })
    }

    /// `Maintenance.Get` of the run `run_id`: each host of the run, in node id order.
    pub fn hosts(&self, run_id: &str) -> Vec<op::MaintenanceHost> {
        let answer: op::GetMaintenanceRunResponse = self
            .rpc(
                GET,
                &op::GetMaintenanceRunRequest {
                    run_id: run_id.to_owned(),
                },
            )
            .expect("the run");
        answer.hosts
    }

    /// Each host of the run `run_id` and its step, in node id order.
    pub fn steps(&self, run_id: &str) -> Vec<(String, op::MaintenanceHostStep)> {
        self.hosts(run_id)
            .iter()
            .map(|host| (host.host_id.clone(), host.step()))
            .collect()
    }

    /// The hosts of the run `run_id` between staging and done.
    pub fn in_turn(&self, run_id: &str) -> Vec<String> {
        self.steps(run_id)
            .into_iter()
            .filter(|(_, step)| IN_TURN.contains(step))
            .map(|(host, _)| host)
            .collect()
    }

    /// `Maintenance.Cancel` of the run `run_id`.
    pub fn cancel(&self, run_id: &str) {
        let _cancelled: op::CancelMaintenanceRunResponse = self
            .rpc(
                CANCEL,
                &op::CancelMaintenanceRunRequest {
                    run_id: run_id.to_owned(),
                    reason: "the window moves".to_owned(),
                },
            )
            .expect("cancelled");
    }

    /// `Maintenance.PutBack` of `host` in the run `run_id`.
    pub fn put_back(&self, run_id: &str, host: &str) {
        let _put: op::PutBackMaintenanceHostResponse = self
            .rpc(
                PUT_BACK,
                &op::PutBackMaintenanceHostRequest {
                    run_id: run_id.to_owned(),
                    host_id: host.to_owned(),
                    reason: "the mirror is back".to_owned(),
                },
            )
            .expect("put back");
    }

    /// The maintenance alerts standing (`updates_due`, `maintenance_halted`,
    /// `maintenance_not_back`), each as its kind, group and host, sorted.
    pub fn alerts(&self) -> Vec<(op::AlertKind, String, String)> {
        let listed = self
            .coordinator()
            .list_alerts(&operator(), op::ListAlertsRequest::default())
            .expect("the alerts");
        let mut alerts: Vec<_> = listed
            .alerts
            .iter()
            .filter(|alert| {
                matches!(
                    alert.kind(),
                    op::AlertKind::UpdatesDue
                        | op::AlertKind::MaintenanceHalted
                        | op::AlertKind::MaintenanceNotBack
                )
            })
            .map(|alert| {
                (
                    alert.kind(),
                    alert.host_group.clone(),
                    alert.host_id.clone(),
                )
            })
            .collect();
        alerts.sort();
        alerts
    }
}
