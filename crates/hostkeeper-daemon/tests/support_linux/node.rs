//! One host of a test: the daemon's launcher over the fake host, its link to the coordinator, and
//! its daemon and heartbeat loop.

use super::*;

/// The daemon's launcher on a fake host: the maintenance hook's steps (`prepare`, `cutover`,
/// `verify`) run in process through the real hook over the host ([`Machine::kinds`]), each
/// service's `drain` and `resume` are the host's service; every other step goes to the rig's
/// scripted launcher.
struct HostLauncher {
    machine: Arc<Machine>,
    inner: Arc<hostkeeper_daemon::testing::ScriptedLauncher>,
}

impl HostLauncher {
    /// The maintenance hook's `step` with the params file `params`, as its unit would run it:
    /// its exit code and its report line. A `prepare` is logged `stage` before it runs.
    fn hook(&self, step: &str, params: &Path) -> StepExit {
        let step = match step {
            "prepare" => {
                self.machine.effect("stage".to_owned());
                HookStep::Prepare
            }
            "cutover" => HookStep::Cutover,
            "verify" => HookStep::Verify,
            other => panic!("the maintenance hook has no step {other}"),
        };
        let mut file = fs::File::open(params).expect("the hook's params");
        let read = MaintainParams::read(&mut file).expect("valid params");
        let record = StoreRecord::beside_params(params).expect("the run record beside the params");
        let kinds = self.machine.kinds();
        let rebooter = HostRebooter::systemd(self.machine.program("systemctl"));
        let ports = HookPorts {
            kinds: &kinds,
            record: &record,
            rebooter: &rebooter,
            boot_id: &*self.machine,
        };
        let ending = run_step(step, &read, &ports);
        if step == HookStep::Verify {
            let line = match ending.reason {
                None => "check".to_owned(),
                Some(reason) => format!("check failed: {reason}"),
            };
            self.machine.effect(line);
        }
        let code = match ending.outcome {
            Outcome::Success => 0,
            Outcome::Deferred => EXIT_DEFERRED,
            _ => 1,
        };
        StepExit {
            code: Some(code),
            last_line: Some(ending.report().to_line()),
        }
    }
}

impl StepLauncher for HostLauncher {
    fn run(&self, call: &StepCall<'_>) -> Result<StepExit, LaunchError> {
        self.inner.run(call)
    }

    fn stop(&self, unit: &str) -> Result<(), LaunchError> {
        self.inner.stop(unit)
    }

    fn run_tool(&self, call: &ToolCall<'_>) -> Result<ToolExit, LaunchError> {
        self.inner.run_tool(call)
    }

    fn run_additive(&self, call: &AdditiveCall<'_>) -> Result<StepExit, LaunchError> {
        if call.service == "hostkeeper" {
            return Ok(self.hook(call.step, call.params));
        }
        let code = self
            .machine
            .service_step(call.service, call.step, call.params);
        Ok(StepExit {
            code: Some(code),
            last_line: None,
        })
    }
}

/// A `maintain` order as a host read it from an answer: its id, try, action and kinds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ordered {
    pub id: String,
    pub order_try: u64,
    pub action: pb::MaintainAction,
    pub kinds: Vec<pb::MaintenanceKind>,
}

/// The network transport of one host, in process: each heartbeat goes through the gRPC router
/// of the coordinator running now, as the host; the request and the `maintain` order of each
/// answer are kept, in order.
struct HostLink {
    world: Arc<World>,
    peer: Peer,
    sent: Mutex<Vec<pb::HeartbeatRequest>>,
    answered: Mutex<Vec<Option<Ordered>>>,
}

impl CoordinatorLink for HostLink {
    fn heartbeat(&self, _identity: &ClientIdentity, beat: &Heartbeat) -> Result<Reply, LinkError> {
        let request = request_of(beat);
        self.sent
            .lock()
            .expect("not poisoned")
            .push(request.clone());
        let (code, _, body) = call(
            &self.world.coordinator(),
            Surface::Heartbeat,
            &self.peer,
            HEARTBEAT,
            None,
            &request.encode_to_vec(),
        );
        if code != 0 {
            return Err(LinkError::Refused(
                Reason::new("forbidden").expect("a token"),
            ));
        }
        let response = pb::HeartbeatResponse::decode(body.as_slice()).expect("a response");
        let maintain = match response.order {
            Some(pb::Order {
                order_id,
                r#try,
                verb: Some(pb::order::Verb::Maintain(maintain)),
            }) => Some(MaintainOrder {
                id: OrderId::new(&order_id).expect("an order id"),
                order_try: r#try,
                maintain,
            }),
            Some(other) => panic!("an order these scenarios do not give: {other:?}"),
            None => None,
        };
        self.answered
            .lock()
            .expect("not poisoned")
            .push(maintain.as_ref().map(|order| Ordered {
                id: order.id.as_str().to_owned(),
                order_try: order.order_try,
                action: order.maintain.action(),
                kinds: order.maintain.kinds().collect(),
            }));
        Ok(Reply {
            features: response
                .features
                .iter()
                .filter_map(|name| Feature::parse(name))
                .collect(),
            maintain,
            ..Reply::default()
        })
    }
}

/// A daemon that runs, and its heartbeat loop.
struct Running {
    heartbeater: Heartbeater,
    dispatch: Arc<ManualDispatch>,
    _started: Option<Started>,
}

/// One host of a test: its daemon's directories, its fake host, its link to the coordinator,
/// and its daemon while one runs.
pub struct Node {
    pub name: String,
    pub rig: Rig,
    pub machine: Arc<Machine>,
    link: Arc<HostLink>,
    running: Option<Running>,
}

impl Node {
    /// The host `host` of the group `group` of the test `test`, running `alpha` 1.0.0 on a fake
    /// host with `due` due; its daemon started.
    pub fn new(world: &Arc<World>, test: &str, host: &str, group: &str, due: Due) -> Node {
        let rig = Rig::new(&format!("{test}-{host}"));
        // Its release names `drain` and `resume`, and a health check the rig's fake client
        // answers healthy, so its heartbeat reports it healthy and the host counts as serving.
        let text = manifest_with(SERVICE, "1.0.0", DRAINS)
            + &format!("\n[health]\nurl = \"{HEALTH_URL}\"\n");
        rig.add_with(SERVICE, "1.0.0", &text);
        rig.store.link(SERVICE, "current", "1.0.0");
        rig.health.answer(HEALTH_URL, 200);
        let machine = Machine::new(&format!("{test}-{host}"), due);
        let link = Arc::new(HostLink {
            world: Arc::clone(world),
            peer: peer(host, group),
            sent: Mutex::new(Vec::new()),
            answered: Mutex::new(Vec::new()),
        });
        let mut node = Node {
            name: host.to_owned(),
            rig,
            machine,
            link,
            running: None,
        };
        assert_eq!(node.start(), None, "a new host has no turn to resume");
        node
    }

    /// The daemon's ports: the rig's, the launcher the fake host's.
    fn ports(&self) -> Ports {
        Ports {
            launcher: Arc::new(HostLauncher {
                machine: Arc::clone(&self.machine),
                inner: Arc::clone(&self.rig.launcher),
            }),
            ..self.rig.ports()
        }
    }

    /// The daemon starts as the host's service manager starts it, and its start resumes the turn
    /// its run record holds: what that resume came to, `None` when there was no turn to resume.
    /// Its heartbeat loop starts afresh: it has heard no coordinator yet.
    pub fn start(&mut self) -> Option<Driven> {
        assert!(self.running.is_none(), "one daemon at a time");
        let mut started =
            start::startup(&self.rig.settings(), self.ports(), OWN).expect("the daemon starts");
        assert_eq!(
            started.boot_report().ready,
            Some(Ok(())),
            "the boot pass ran"
        );
        let resumed = started.join_maintenance();
        let daemon = Arc::clone(started.daemon());
        self.run(daemon, Some(started));
        resumed
    }

    /// A daemon whose run record's writer stops at the first write `stop` says yes to, as a
    /// crash there would, starts with no boot pass and no resume.
    pub fn start_crashing(&mut self, stop: CrashGate) {
        assert!(self.running.is_none(), "one daemon at a time");
        let daemon = Daemon::new(config(), self.rig.dirs(), self.ports(), OWN)
            .with_maintenance_crash_gate(stop);
        self.run(Arc::new(daemon), None);
    }

    fn run(&mut self, daemon: Arc<Daemon>, started: Option<Started>) {
        let dispatch = Arc::new(ManualDispatch::default());
        let log = Arc::new(RecordingLog::default());
        let clock = Arc::new(LinkClock::new(2_000_000_000, 7));
        let maintenance = maintenance_source(
            &daemon,
            LinuxScope::Security,
            self.machine.collectors(),
            clock.clone(),
            dispatch.clone(),
            log.clone(),
        );
        assert!(maintenance.is_some(), "the host reports its maintenance");
        let links = Links {
            enroller: Box::new(|| Err("the scenario has no enrollment".to_owned())),
            link: self.link.clone(),
            certs: Arc::new(FakeCerts::new(CertState::Enrolled(identity(&self.name)))),
            clock,
            log,
            sleeper: Box::new(ScriptedSleeper::new(0)),
            period: DEFAULT_HEARTBEAT_SECS,
            dispatch: dispatch.clone(),
            stop: StopFlag::default(),
            enroll_sleeper: Box::new(ScriptedSleeper::new(0)),
            renewal: hostkeeper_enroll::RenewalState::default(),
            watchdog: Box::new(hostkeeper_daemon::NoWatchdog),
            console: None,
            identity: None,
            remote_login_probe: None,
            full_disk_access: None,
            operator_keys: None,
            prerequisites: None,
            maintenance,
            bmc: None,
            serial_console: None,
        };
        self.running = Some(Running {
            heartbeater: links.heartbeater(&daemon),
            dispatch,
            _started: started,
        });
    }

    /// The daemon and its loop end, as a crash or a reboot ends them.
    pub fn stop(&mut self) {
        assert!(self.running.take().is_some(), "a daemon runs");
    }

    /// One heartbeat, answered, then every job it started (an order's turn, a look of the
    /// host's updates) run to its end: the `maintain` order of the answer, if any.
    pub fn beat(&mut self) -> Option<Ordered> {
        let running = self.running.as_mut().expect("a daemon runs");
        assert_eq!(running.heartbeater.beat_once(), BeatOutcome::Sent);
        while running.dispatch.run_next() {}
        self.link
            .answered
            .lock()
            .expect("not poisoned")
            .last()
            .cloned()
            .flatten()
    }

    /// Heartbeats until the coordinator has heard the host's report of its maintenance: the
    /// first heartbeat learns that the coordinator reads one, the second carries it and starts a
    /// look of the host's updates, the third carries what the look found.
    pub fn report(&mut self) {
        for _ in 0..3 {
            assert_eq!(self.beat(), None, "no order before a run is scheduled");
        }
    }

    /// The host reboots as it asked: the daemon and its loop end, the host boots into a new boot
    /// id, and a new daemon starts on the same directories: what its resume came to.
    pub fn reboot(&mut self) -> Option<Driven> {
        self.stop();
        let id = self.machine.boot();
        self.rig.clock.set_boot_id(&id);
        self.start()
    }

    /// The maintenance report of the last heartbeat.
    pub fn last_report(&self) -> pb::MaintenanceReport {
        self.link
            .sent
            .lock()
            .expect("not poisoned")
            .last()
            .and_then(|request| request.maintenance.clone())
            .expect("the last heartbeat reported its maintenance")
    }

    /// The kinds the last heartbeat's report lists as due, in its order.
    pub fn due(&self) -> Vec<pb::MaintenanceKind> {
        self.last_report()
            .due
            .iter()
            .map(|item| item.kind())
            .collect()
    }

    /// Every `maintain` order the host was answered, in order.
    pub fn orders(&self) -> Vec<Ordered> {
        self.link
            .answered
            .lock()
            .expect("not poisoned")
            .iter()
            .flatten()
            .cloned()
            .collect()
    }

    /// The daemon's run record, `None` when there is none.
    pub fn record(&self) -> Option<MaintenanceRecord> {
        match MaintenanceStore::beside(&self.rig.state).load() {
            Ok(MaintenanceLoaded::Current(record)) => Some(*record),
            _ => None,
        }
    }
}

/// A crash of the run record's writer just after the write that records `service`'s drain
/// ended: the record on disk then holds it drained, and nothing else launched.
pub fn after_drained(node: &Node, service: &str) -> CrashGate {
    let state = node.rig.state.clone();
    let service = service.to_owned();
    Arc::new(move |point: CrashPoint| {
        if !matches!(point, CrashPoint::AfterWrite(_)) {
            return false;
        }
        match MaintenanceStore::beside(&state).load() {
            Ok(MaintenanceLoaded::Current(record)) => {
                !record.running && record.drained.iter().any(|name| name.as_str() == service)
            }
            _ => false,
        }
    })
}
