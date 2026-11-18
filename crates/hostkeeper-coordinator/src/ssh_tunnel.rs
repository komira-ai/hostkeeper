//! The tunnels an instance holds (docs/design/ssh-through-coordinator.md, sections 7, 8 and 10):
//! the byte stream between an operator's client and a host's sshd, the registry of the tunnels
//! this instance holds, the poll that closes the ones that must close, and the drain at stop.
//!
//! What a tunnel is allowed is settled before it exists ([`crate::ssh_access`]); the book
//! ([`crate::ssh_book`]) holds the rest of the fleet's view of it (one tunnel per operator per
//! host over every instance, the audit lines). Here the instance holds what only it can: the
//! socket to the host, which it relays in frames of at most [`MAX_FRAME`] bytes.
//!
//! The relay moves bytes as fast as the slower side takes them. The host is read only when a slot
//! of the queue towards the client ([`QUEUE`] frames) is reserved, and the client is read only
//! when what it sent last has been written to the host, so a client that reads nothing makes the
//! host wait in its own socket and never makes the coordinator hold more than the queue. A
//! `close` from the client ends its input only (the write half of the host's socket is shut
//! down, as `ssh` does at the end of an `scp`) and the host's answer is still relayed until the
//! host closes; a host that closes ends the tunnel (`host_closed`), as do the timers (`idle`,
//! `max_session`), the poll (`token_revoked`, which is an operator token that is gone or, for the tunnel
//! of a UI terminal, a login session that ended; `not_permitted`, `revoked`, `instance_lost`) and
//! the drain (`coordinator_stopping`). Whatever ends it, the client is sent `close{cause}` and
//! the book is told the bytes each way, so the audit holds the cause.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use futures::stream::BoxStream;
use hostkeeper_proto::hostkeeper::operator::v1 as proto;
use hostkeeper_proto::hostkeeper::operator::v1::tunnel_frame::Frame;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::{self, Sender};
use tokio::sync::{Notify, watch};
use tokio::time::{Instant, sleep_until, timeout};
use tonic::Status;

use crate::enrollment::Random;
use crate::service::Coordinator;
use crate::session::SessionKey;
use crate::ssh_book::{Book, Cause, Closing, OpenRefused, Opened, Opening};
use crate::ssh_operators::{Operators, Target};
use crate::ssh_sign::{Signing, class_of};
use crate::wait::ConnectionHold;

/// The most bytes of one `data` frame, either way.
pub(crate) const MAX_FRAME: usize = 16 * 1024;

/// The frames the queue towards the client holds: the most the coordinator reads from the host
/// ahead of a client that reads nothing.
pub(crate) const QUEUE: usize = 4;

/// How long the drain waits for the closes of the tunnels it stopped to be written in the book.
pub(crate) const CLOSE_GRACE: Duration = Duration::from_secs(5);

/// What the client sends: the frames of its stream.
pub(crate) type Incoming = BoxStream<'static, Result<proto::TunnelFrame, Status>>;

/// What the client is sent: the frames of the response stream.
pub(crate) type Outgoing = BoxStream<'static, Result<proto::TunnelFrame, Status>>;

/// The limits of the tunnels, from the flags of `--ssh-*` (design, section 10).
#[derive(Clone, Debug)]
pub(crate) struct Limits {
    /// How recent a host's last heartbeat must be, in seconds (`--ssh-live-s`).
    pub(crate) live_secs: u64,
    /// How long a tunnel may carry no byte either way (`--ssh-idle-s`).
    pub(crate) idle: Duration,
    /// How long a tunnel may stay open (`--ssh-max-session-s`).
    pub(crate) max_session: Duration,
    /// How long the connection to the host may take (`--ssh-dial-timeout-s`).
    pub(crate) dial_timeout: Duration,
    /// The most tunnels this instance holds (`--ssh-max-tunnels`).
    pub(crate) max_tunnels: usize,
    /// The most tunnels an operator holds over every instance
    /// (`--ssh-max-tunnels-per-operator`).
    pub(crate) per_operator: u64,
    /// How often the poll runs (`--ssh-revoke-poll-s`).
    pub(crate) poll: Duration,
    /// How long a stop waits for the tunnels to end on their own (`--ssh-drain-s`).
    pub(crate) drain: Duration,
    /// The port of the host's sshd: 22, and in a test the port of a listener of its own. The one
    /// place the dial is told where to go.
    pub(crate) port: u16,
}

/// What a test sees of the tunnels: progress events, so a test waits for what it needs instead
/// of for time.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// Tunnel `id` was registered and its `opened` frame queued.
    Opened(u64),
    /// The relay of tunnel `id` runs, its timers set.
    Relaying(u64),
    /// A frame of the host's bytes was queued towards the client of tunnel `id`.
    Down(u64),
    /// The queue towards the client of tunnel `id` was full: the host is not read until the
    /// client takes a frame.
    Waiting(u64),
    /// Tunnel `id` ended with `cause`: the book holds its close and the registry no longer
    /// lists it.
    Closed(u64, Cause),
}

/// What the operator of a tunnel is acting under, which the poll holds the tunnel to: the tunnel
/// ends `token_revoked` when this is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Credential {
    /// An operator token (`hkssh`): the operator must still have one.
    Token,
    /// A signed-in session (the UI's terminal, `crate::ssh_terminal`): the session must still be
    /// live. The operator may hold no token at all.
    Session(SessionKey),
}

/// A tunnel of this instance: who holds it and the way to stop it.
struct Entry {
    operator: String,
    host_id: String,
    credential: Credential,
    stop: watch::Sender<Option<Cause>>,
}

impl Entry {
    /// Stops the tunnel with `cause`, unless it was stopped already: the first cause stands.
    fn stop(&self, cause: Cause) {
        self.stop.send_if_modified(|held| {
            let free = held.is_none();
            if free {
                *held = Some(cause);
            }
            free
        });
    }
}

/// What the instance holds, and whether it takes more.
#[derive(Default)]
struct Registry {
    entries: BTreeMap<u64, Entry>,
    /// Slots taken by a tunnel that is being opened and is not registered yet.
    pending: usize,
    /// The ids of the rows the book holds for tunnels that are not registered yet: from the
    /// book's `open` until [`Slot::register`] or the close of the row. The poll refreshes them.
    recorded: BTreeSet<u64>,
    /// Set by the drain: no tunnel is opened any more.
    draining: bool,
    /// Set when the drain gave up waiting: a tunnel that registers now is stopped at once.
    stopping: bool,
}

impl Registry {
    fn idle(&self) -> bool {
        self.entries.is_empty() && self.pending == 0
    }
}

/// Why no slot was given for a tunnel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoSlot {
    /// The instance is draining.
    Draining,
    /// The instance holds `--ssh-max-tunnels` already.
    Full,
}

/// A tunnel the book recorded and no relay holds yet: closed in the book, as `error`, when it is
/// dropped before [`Recorded::keep`], so that a caller that gives up while the host is dialled
/// leaves no row behind (which would keep the operator from the host until the instance stops).
pub(crate) struct Recorded {
    tunnels: Arc<Tunnels>,
    opened: Opened,
    armed: bool,
}

impl Recorded {
    /// Closes the row now, as `cause`, and waits for the book.
    async fn close(mut self, cause: Cause) {
        self.armed = false;
        let book = Arc::clone(&self.tunnels.book);
        let closing = closing_of(self.opened.id, cause);
        match tokio::task::spawn_blocking(move || book.close(&closing)).await {
            Ok(Ok(())) => {}
            Ok(Err(why)) => tracing::error!(%why, "the close of an unused tunnel was not written"),
            Err(why) => tracing::error!(%why, "the close of an unused tunnel failed"),
        }
        // Closed or not: a row left is no longer refreshed, and the poll's reaper takes it.
        self.tunnels.unrecord(self.opened.id);
    }

    /// The tunnel is held by a relay, which closes it: gives what the book recorded.
    pub(crate) fn keep(mut self) -> Opened {
        self.armed = false;
        self.opened.clone()
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        if self.armed {
            self.tunnels.unrecord(self.opened.id);
            let book = Arc::clone(&self.tunnels.book);
            let closing = closing_of(self.opened.id, Cause::Error);
            drop(tokio::task::spawn_blocking(move || {
                if let Err(why) = book.close(&closing) {
                    tracing::error!(%why, "the close of an abandoned tunnel was not written");
                }
            }));
        }
    }
}

/// The close of tunnel `id` that moved no byte.
fn closing_of(id: u64, cause: Cause) -> Closing {
    Closing {
        id,
        cause,
        bytes_up: 0,
        bytes_down: 0,
    }
}

/// A tunnel recorded in the book and connected to its host, with the slot it takes: the slot
/// stays counted until whoever holds this lets go of it, so a caller that gave up does not free
/// the slot before the row is closed.
pub(crate) struct Dialed {
    pub(crate) recorded: Recorded,
    pub(crate) host: TcpStream,
    pub(crate) slot: Slot,
}

/// Why a tunnel was not opened and connected.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Failed {
    /// The book did not record it.
    Open(OpenRefused),
    /// The host's sshd could not be reached: the book's row is closed (`error`).
    Dial,
}

/// A tunnel the registry holds, as the poll reads it.
struct Held {
    id: u64,
    operator: String,
    host_id: String,
    credential: Credential,
}

/// The tunnels of one coordinator instance.
pub(crate) struct Tunnels {
    pub(crate) book: Arc<dyn Book>,
    instance: String,
    pub(crate) limits: Limits,
    registry: Mutex<Registry>,
    /// Told when the registry has no tunnel and no slot left.
    empty: Notify,
    #[cfg(test)]
    events: Mutex<Option<mpsc::UnboundedSender<Event>>>,
}

/// The id of an instance: 16 lowercase hex characters of `random`'s bytes, made at start.
///
/// Returns `None` when `random` has no bytes to give.
pub(crate) fn instance_id(random: &Random) -> Option<String> {
    let mut bytes = [0_u8; 8];
    random(&mut bytes).then(|| bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

impl Tunnels {
    /// The tunnels of the instance `instance` over `book`.
    pub(crate) fn new(book: Arc<dyn Book>, instance: String, limits: Limits) -> Self {
        Self {
            book,
            instance,
            limits,
            registry: Mutex::new(Registry::default()),
            empty: Notify::new(),
            #[cfg(test)]
            events: Mutex::new(None),
        }
    }

    /// The id of this instance, which the book records on each tunnel it holds.
    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }

    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Wakes whoever waits for the registry to be empty, if it is.
    fn settle(&self, registry: &Registry) {
        if registry.idle() {
            self.empty.notify_waiters();
        }
    }

    /// Tells a test's reader `event`.
    #[cfg(test)]
    fn tell(&self, event: Event) {
        let events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(events) = events.as_ref() {
            let _ = events.send(event);
        }
    }

    /// The events of the tunnels from now on, for a test.
    #[cfg(test)]
    pub(crate) fn watch(&self) -> mpsc::UnboundedReceiver<Event> {
        let (told, events) = mpsc::unbounded_channel();
        *self.events.lock().unwrap_or_else(PoisonError::into_inner) = Some(told);
        events
    }

    /// Whether the instance takes no new tunnel.
    pub(crate) fn draining(&self) -> bool {
        self.registry().draining
    }

    /// The tunnels held, with their operators and hosts, and the ids of the rows of those being
    /// opened.
    fn held(&self) -> (Vec<Held>, Vec<u64>) {
        let registry = self.registry();
        let held = registry
            .entries
            .iter()
            .map(|(id, entry)| Held {
                id: *id,
                operator: entry.operator.clone(),
                host_id: entry.host_id.clone(),
                credential: entry.credential,
            })
            .collect();
        (held, registry.recorded.iter().copied().collect())
    }

    /// How many slots are taken by tunnels being opened.
    #[cfg(test)]
    pub(crate) fn slots(&self) -> usize {
        self.registry().pending
    }

    /// How many tunnels are registered.
    #[cfg(test)]
    pub(crate) fn count(&self) -> usize {
        self.registry().entries.len()
    }

    /// A slot for one tunnel to be opened.
    ///
    /// # Errors
    /// [`NoSlot::Draining`] once the drain began; [`NoSlot::Full`] at `max_tunnels`.
    pub(crate) fn reserve(self: &Arc<Self>) -> Result<Slot, NoSlot> {
        let mut registry = self.registry();
        if registry.draining {
            return Err(NoSlot::Draining);
        }
        if registry.entries.len() + registry.pending >= self.limits.max_tunnels {
            return Err(NoSlot::Full);
        }
        registry.pending += 1;
        Ok(Slot {
            tunnels: Arc::clone(self),
            used: false,
        })
    }

    /// Stops tunnel `id` with `cause`, if it is held and not stopped already: the first cause
    /// stands.
    fn stop(&self, id: u64, cause: Cause) {
        if let Some(entry) = self.registry().entries.get(&id) {
            entry.stop(cause);
        }
    }

    /// The row `id` is no longer one of a tunnel being opened.
    fn unrecord(&self, id: u64) {
        self.registry().recorded.remove(&id);
    }

    /// Takes tunnel `id` out of the registry.
    fn deregister(&self, id: u64) {
        let mut registry = self.registry();
        registry.entries.remove(&id);
        self.settle(&registry);
    }

    /// Waits until the registry holds no tunnel and no slot.
    async fn emptied(&self) {
        loop {
            let woken = self.empty.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            if self.registry().idle() {
                return;
            }
            woken.await;
        }
    }

    /// The connection to `address` (an IP address) at the sshd's port, within `dial_timeout`.
    pub(crate) async fn dial(&self, address: &str) -> Option<TcpStream> {
        let Ok(ip) = address.parse::<IpAddr>() else {
            tracing::warn!(%address, "the address of a host is no IP address");
            return None;
        };
        let to = SocketAddr::new(ip, self.limits.port);
        self.connected(to, TcpStream::connect(to)).await
    }

    /// The connection `connecting` makes to `to`, if it is made within `dial_timeout`.
    async fn connected(
        &self,
        to: SocketAddr,
        connecting: impl Future<Output = io::Result<TcpStream>>,
    ) -> Option<TcpStream> {
        match timeout(self.limits.dial_timeout, connecting).await {
            Ok(Ok(stream)) => {
                // A keystroke is a small write: it goes out now. A failure costs latency only.
                let _ = stream.set_nodelay(true);
                Some(stream)
            }
            Ok(Err(why)) => {
                tracing::warn!(%to, %why, "a host's sshd could not be reached");
                None
            }
            Err(_) => {
                tracing::warn!(%to, "a host's sshd did not answer in time");
                None
            }
        }
    }

    /// Records the tunnel in the book and connects to the host it names. Run as a task of its own
    /// by the call that opens a tunnel, so that a caller that gives up midway does not leave a
    /// row in the book without a tunnel: [`Recorded`] closes it.
    ///
    /// The job owns `slot` until it is done: it comes back in [`Dialed`], and is given back when
    /// the job fails or its result is dropped.
    pub(crate) async fn open_and_dial(
        self: Arc<Self>,
        opening: Opening,
        slot: Slot,
    ) -> Result<Dialed, Failed> {
        let book = Arc::clone(&self.book);
        let recording = tokio::task::spawn_blocking(move || book.open(&opening))
            .await
            .unwrap_or_else(|why| Err(OpenRefused::Unwritten(why.to_string())));
        let opened = recording.map_err(Failed::Open)?;
        self.registry().recorded.insert(opened.id);
        let recorded = Recorded {
            tunnels: Arc::clone(&self),
            opened,
            armed: true,
        };
        match self.dial(&recorded.opened.address).await {
            Some(host) => Ok(Dialed {
                recorded,
                host,
                slot,
            }),
            None => {
                recorded.close(Cause::Error).await;
                Err(Failed::Dial)
            }
        }
    }

    /// Registers the tunnel `opened` in `slot` for `who` (operator, host, and the credential the
    /// poll holds the tunnel to), queues its `opened` frame and starts the relay
    /// between `client` and `host`. `hold` keeps the client's connection for as long as the relay
    /// runs.
    pub(crate) fn start(
        self: &Arc<Self>,
        slot: Slot,
        opened: &Opened,
        who: (&str, &str, Credential),
        login_user: &str,
        streams: (TcpStream, Incoming),
        hold: Option<ConnectionHold>,
    ) -> Outgoing {
        let (operator, host_id, credential) = who;
        let (host, client) = streams;
        let (out, queued) = mpsc::channel(QUEUE);
        // The queue is empty and holds `QUEUE` frames: the first cannot be refused.
        let _ = out.try_send(proto::TunnelFrame {
            frame: Some(Frame::Opened(proto::Opened {
                login_user: login_user.to_owned(),
            })),
        });
        let stop = slot.register(opened.id, (operator, host_id), credential);
        #[cfg(test)]
        self.tell(Event::Opened(opened.id));
        tokio::spawn(relay(
            Arc::clone(self),
            Relaying {
                id: opened.id,
                host,
                client,
                out,
                stop,
            },
            hold,
        ));
        futures::stream::unfold(queued, |mut queued| async move {
            queued.recv().await.map(|frame| (Ok(frame), queued))
        })
        .boxed()
    }

    /// One turn of the poll: stops the tunnels that must not go on, and writes the book's view
    /// of this instance (its last-seen time, the reaping of lost instances).
    ///
    /// A tunnel stops with `token_revoked` when its credential is gone: its operator has no token
    /// any more (neither in the tokens file nor among the API tokens), or, for the tunnel of a
    /// UI terminal, the session it runs under no longer lives (signed out, expired, its user
    /// removed or disabled); with `not_permitted` when the operators file no
    /// longer lists and permits the operator the host or the host is no longer enrolled, with
    /// `revoked` when the book says its certificate was revoked, and with `instance_lost` when
    /// the book no longer holds it (another instance's reaper closed it). An operators file that
    /// cannot be read stops none for `not_permitted`. The poll passes the book the ids of the
    /// tunnels held and of those being opened (recorded, not registered), so the book refreshes
    /// them and reaps every other row that went stale, this instance's own included.
    pub(crate) async fn poll_once(&self, coordinator: &Coordinator) {
        let Some(ssh) = coordinator.ssh.as_ref() else {
            return;
        };
        let (held, recorded) = self.held();
        // A file that cannot be read keeps the tunnels, as a host that cannot be read does: the
        // next poll reads it again.
        let operators = ssh.read_operators().inspect_err(|why| {
            tracing::warn!(%why, "the operators file could not be read: its tunnels are kept");
        });
        for tunnel in &held {
            let Held {
                id,
                operator,
                host_id,
                credential,
            } = tunnel;
            if !credential_holds(coordinator, operator, *credential) {
                self.stop(*id, Cause::TokenRevoked);
            } else if let Ok(operators) = &operators
                && !permitted(coordinator, ssh, operators, operator, host_id)
            {
                self.stop(*id, Cause::NotPermitted);
            }
        }
        // The tunnels held and those being opened: the book refreshes both and reaps the rest.
        let ids: Vec<u64> = held
            .iter()
            .map(|tunnel| tunnel.id)
            .chain(recorded)
            .collect();
        let stale = self.limits.poll.as_secs().saturating_mul(3);
        let book = Arc::clone(&self.book);
        let instance = self.instance.clone();
        let polled = tokio::task::spawn_blocking(move || book.poll(&instance, &ids, stale)).await;
        match polled {
            Ok(Ok(polled)) => {
                if polled.reaped > 0 {
                    tracing::info!(
                        reaped = polled.reaped,
                        "closed the tunnels of instances that were lost"
                    );
                }
                for id in polled.revoked {
                    self.stop(id, Cause::Revoked);
                }
                for id in polled.gone {
                    self.stop(id, Cause::InstanceLost);
                }
            }
            Ok(Err(why)) => tracing::warn!(%why, "the book of the tunnels was not polled"),
            Err(why) => tracing::warn!(%why, "the poll of the book of the tunnels failed"),
        }
    }

    /// [`Self::poll_once`] every `poll`, for as long as it is polled.
    pub(crate) async fn poll_loop(self: Arc<Self>, coordinator: Arc<Coordinator>) -> Infallible {
        let mut ticks = tokio::time::interval(self.limits.poll);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            self.poll_once(&coordinator).await;
        }
    }

    /// Stops taking tunnels, waits until the tunnels held end on their own or `drain` passes,
    /// then stops the rest with `coordinator_stopping` and waits for them to be closed in the
    /// book, for [`CLOSE_GRACE`] at most.
    pub(crate) async fn drain(&self) {
        self.registry().draining = true;
        if timeout(self.limits.drain, self.emptied()).await.is_err() {
            self.stop_all();
            if timeout(CLOSE_GRACE, self.emptied()).await.is_err() {
                tracing::error!(
                    "the tunnels did not close in the book in time: the stop goes on and the rows are left to another instance's poll"
                );
            }
        }
    }

    /// The drain gave up waiting: stops every tunnel held, and every one that registers from now
    /// on, with `coordinator_stopping`.
    fn stop_all(&self) {
        let mut registry = self.registry();
        registry.stopping = true;
        for entry in registry.entries.values() {
            entry.stop(Cause::CoordinatorStopping);
        }
    }
}

/// Whether the credential a tunnel of `operator` runs under is still good: for a token, that the
/// operator has one; for a session, that the session lives (asked without using it).
fn credential_holds(coordinator: &Coordinator, operator: &str, credential: Credential) -> bool {
    match credential {
        Credential::Token => has_token(coordinator, operator),
        Credential::Session(key) => coordinator
            .login()
            .is_some_and(|login| login.sessions.is_live(&key)),
    }
}

/// Whether the operator `name` has a token: one of the tokens file, or an API token.
pub(crate) fn has_token(coordinator: &Coordinator, name: &str) -> bool {
    coordinator.tokens().has_name(name)
        || coordinator
            .accounts()
            .read(|accounts| accounts.token(name).is_some())
}

/// Whether the operators file as read (`operators`) lists `operator` and permits it the host
/// `host_id`, which must still be enrolled. A host that cannot be read keeps the tunnel: the
/// next poll reads it again.
fn permitted(
    coordinator: &Coordinator,
    ssh: &Signing,
    operators: &Operators,
    operator: &str,
    host_id: &str,
) -> bool {
    match coordinator.enrolled_host(host_id) {
        Ok(Some(host)) => {
            operators.lists(operator)
                && operators.permits(
                    operator,
                    &Target {
                        id: host_id,
                        group: &host.host_group,
                        class: class_of(ssh, host_id),
                    },
                )
        }
        Ok(None) => false,
        Err(why) => {
            tracing::warn!(%why, "a host could not be read: its tunnels are kept");
            true
        }
    }
}

/// A place for one tunnel that is being opened: given back when it is dropped, and used up by
/// [`Slot::register`].
pub(crate) struct Slot {
    tunnels: Arc<Tunnels>,
    used: bool,
}

impl Slot {
    /// Registers tunnel `id` of `operator` to `host_id` in the place of the slot, and gives the
    /// signal that stops it. A tunnel that registers after the drain gave up is stopped already.
    fn register(
        mut self,
        id: u64,
        who: (&str, &str),
        credential: Credential,
    ) -> watch::Receiver<Option<Cause>> {
        let (operator, host_id) = who;
        let mut registry = self.tunnels.registry();
        registry.pending -= 1;
        registry.recorded.remove(&id);
        let first = registry.stopping.then_some(Cause::CoordinatorStopping);
        let (stop, signal) = watch::channel(first);
        registry.entries.insert(
            id,
            Entry {
                operator: operator.to_owned(),
                host_id: host_id.to_owned(),
                credential,
                stop,
            },
        );
        self.used = true;
        signal
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if !self.used {
            let mut registry = self.tunnels.registry();
            registry.pending -= 1;
            self.tunnels.settle(&registry);
        }
    }
}

/// What a relay holds: the host, the client's frames, the queue towards the client and the signal
/// that stops it.
struct Relaying {
    id: u64,
    host: TcpStream,
    client: Incoming,
    out: Sender<proto::TunnelFrame>,
    stop: watch::Receiver<Option<Cause>>,
}

/// How a relay ended.
struct End {
    cause: Cause,
    up: u64,
    down: u64,
}

/// The frame that tells the client the stream is over, and why.
fn close_frame(cause: Cause) -> proto::TunnelFrame {
    proto::TunnelFrame {
        frame: Some(Frame::Close(proto::Close {
            cause: cause.as_str().to_owned(),
        })),
    }
}

/// Runs a tunnel to its end, writes its close in the book, takes it out of the registry and
/// tells the client why.
async fn relay(tunnels: Arc<Tunnels>, relaying: Relaying, hold: Option<ConnectionHold>) {
    let mut relaying = relaying;
    let end = pump(&tunnels, &mut relaying).await;
    let Relaying {
        id,
        host,
        client,
        out,
        stop,
    } = relaying;
    // The host's socket and the client's stream are over; the rest is the audit.
    drop((host, client, stop));
    tracing::info!(
        id,
        cause = end.cause.as_str(),
        up = end.up,
        down = end.down,
        "a tunnel ended"
    );
    // The book first: the audit is what must not be lost. A tunnel whose row another instance
    // closed has its line already.
    if end.cause != Cause::InstanceLost {
        let book = Arc::clone(&tunnels.book);
        let closing = Closing {
            bytes_up: end.up,
            bytes_down: end.down,
            ..closing_of(id, end.cause)
        };
        match tokio::task::spawn_blocking(move || book.close(&closing)).await {
            Ok(Ok(())) => {}
            Ok(Err(why)) => tracing::error!(%why, id, "the close of a tunnel was not written"),
            Err(why) => tracing::error!(%why, id, "the close of a tunnel failed"),
        }
    }
    tunnels.deregister(id);
    #[cfg(test)]
    tunnels.tell(Event::Closed(id, end.cause));
    // A client that has gone has no use for it; one that reads nothing is cut by its connection.
    let _ = out.send(close_frame(end.cause)).await;
    drop(hold);
}

/// The cause a stop signal carries, once it does. A signal whose sender is gone is an error.
async fn stopped(stop: &mut watch::Receiver<Option<Cause>>) -> Cause {
    stop.wait_for(Option::is_some)
        .await
        .ok()
        .and_then(|cause| *cause)
        .unwrap_or(Cause::Error)
}

/// Moves the bytes of a tunnel until something ends it.
async fn pump(tunnels: &Tunnels, relaying: &mut Relaying) -> End {
    let limits = &tunnels.limits;
    let Relaying {
        id,
        host,
        client,
        out,
        stop,
    } = relaying;
    let (mut from_host, mut to_host) = host.split();
    let started = Instant::now();
    let session_end = started + limits.max_session;
    let mut idle_end = started + limits.idle;
    #[cfg(test)]
    tunnels.tell(Event::Relaying(*id));
    let mut buffer = vec![0_u8; MAX_FRAME];
    // What the client sent and is not written to the host yet, and how much of it is.
    let mut pending: Vec<u8> = Vec::new();
    let mut written = 0;
    let (mut up, mut down) = (0_u64, 0_u64);
    // Until the client's `close`: after it the client is not read, only the host is.
    let mut client_open = true;
    let cause = loop {
        tokio::select! {
            biased;
            cause = stopped(stop) => break cause,
            () = sleep_until(session_end) => break Cause::MaxSession,
            () = sleep_until(idle_end) => break Cause::Idle,
            () = out.closed() => break Cause::Error,
            sent = client.next(), if client_open && pending.is_empty() => match sent {
                Some(Ok(proto::TunnelFrame { frame: Some(Frame::Data(bytes)) }))
                    if bytes.len() <= MAX_FRAME =>
                {
                    pending = bytes;
                    written = 0;
                }
                Some(Ok(proto::TunnelFrame { frame: Some(Frame::Close(_)) })) => {
                    client_open = false;
                    if to_host.shutdown().await.is_err() {
                        break Cause::Error;
                    }
                }
                None => break Cause::Eof,
                // A stream that failed, and a frame the client has no right to send: a second
                // `open`, an `opened`, no frame at all, or more than a frame's bytes.
                Some(_) => break Cause::Error,
            },
            wrote = to_host.write(&pending[written..]), if written < pending.len() => match wrote {
                Ok(n) => {
                    written += n;
                    up += n as u64;
                    idle_end = Instant::now() + limits.idle;
                    if written == pending.len() {
                        pending.clear();
                        written = 0;
                    }
                }
                Err(why) => {
                    tracing::debug!(id, %why, "a host did not take the bytes of its tunnel");
                    break Cause::Error;
                }
            },
            read = async {
                let permit = match out.try_reserve() {
                    Ok(permit) => permit,
                    Err(TrySendError::Closed(())) => return None,
                    Err(TrySendError::Full(())) => {
                        #[cfg(test)]
                        tunnels.tell(Event::Waiting(*id));
                        out.reserve().await.ok()?
                    }
                };
                Some((permit, from_host.read(&mut buffer).await))
            } => match read {
                None => break Cause::Error,
                Some((_, Ok(0))) => break Cause::HostClosed,
                Some((permit, Ok(n))) => {
                    permit.send(proto::TunnelFrame { frame: Some(Frame::Data(buffer[..n].to_vec())) });
                    down += n as u64;
                    idle_end = Instant::now() + limits.idle;
                    #[cfg(test)]
                    tunnels.tell(Event::Down(*id));
                }
                Some((_, Err(why))) => {
                    tracing::debug!(id, %why, "a host's bytes could not be read");
                    break Cause::Error;
                }
            },
        }
    };
    End { cause, up, down }
}

/// The loop of the poll over `coordinator`'s tunnels, for the listeners to run beside their own
/// loops: it never ends, and waits for ever on a coordinator without tunnels.
pub(crate) async fn run_poll(coordinator: Arc<Coordinator>) -> Infallible {
    match coordinator.tunnels.clone() {
        Some(tunnels) => tunnels.poll_loop(coordinator).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
#[path = "ssh_tunnel_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "ssh_tunnel_poll_tests.rs"]
mod poll_tests;

#[cfg(test)]
#[path = "ssh_tunnel_serve_tests.rs"]
mod serve_tests;
