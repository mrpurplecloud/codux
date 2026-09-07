use crate::{
    RemoteControllerTransportConfig, RemoteHostTransportConfig, RemoteHostTransportHandlers,
    RemoteTransport, RemoteTransportAuthorizationHandler, RemoteTransportCandidate,
    RemoteTransportLogHandler, RemoteTransportMessageHandler, RemoteTransportPairingHandler,
    RemoteTransportStateHandler, RemoteTransportUpload, RemoteTransportUploadHandler,
    RemoteTransportUploadOutcome, WebTunnelIoStream, WebTunnelResponse, WebTunnelTcpConnectHandler,
    WebTunnelTcpConnectRequest,
    web_tunnel::{
        CODUX_WEB_TUNNEL_ALPN, WEB_TUNNEL_KIND_TCP_CONNECT, WebTunnelHeader,
        WebTunnelRequestEnvelope, WebTunnelResponseEnvelope,
    },
};
use async_trait::async_trait;
use codux_protocol::{
    REMOTE_BLOB_MAX_BYTES, REMOTE_TERMINAL_UPLOAD_BLOB, RemoteEnvelope, RemoteTransportKind,
};
use futures_util::StreamExt;
use iroh::{
    Endpoint, EndpointAddr, RelayMap, RelayMode, RelayUrl, SecretKey, TransportAddr, Watcher,
    endpoint::{Connection, PathEvent, RecvStream, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_blobs::{
    BlobsProtocol, HashAndFormat,
    store::{
        GcConfig,
        mem::{MemStore, Options as MemStoreOptions},
    },
    ticket::BlobTicket,
};
use iroh_mdns_address_lookup::MdnsAddressLookup;
use iroh_tickets::endpoint::EndpointTicket;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{Mutex as AsyncMutex, mpsc};

pub const CODUX_REMOTE_ALPN: &[u8] = b"/codux/remote/1";
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const STREAM_KIND_CONTROL: u8 = 0;
const STREAM_KIND_TERMINAL: u8 = 1;
const IROH_RELAY_URL_ENV: &str = "CODUX_IROH_RELAY_URL";
/// Set (non-empty) to disable LAN mDNS address lookup. Direct LAN discovery is
/// best-effort and additive to the relay path, so disabling it only forgoes
/// same-network direct connections — useful on networks that block multicast.
const IROH_DISABLE_LAN_MDNS_ENV: &str = "CODUX_IROH_DISABLE_LAN_MDNS";
const IROH_ONLINE_TIMEOUT: Duration = Duration::from_secs(12);
const BLOB_GC_INTERVAL: Duration = Duration::from_secs(30);
const BLOB_TICKET_LEASE: Duration = Duration::from_secs(5 * 60);
const CONTROL_QUEUE_CAPACITY: usize = 64;
const TERMINAL_QUEUE_CAPACITY: usize = 256;
const UPLOAD_QUEUE_CAPACITY: usize = 2;
/// Upper bound on a SINGLE controller→host dial attempt. Kept short on purpose:
/// the first relay-routed dial to a peer routinely stalls while the relay primes
/// the cross-path between the two nodes (measured: first dial times out, the very
/// next one connects in ~18ms), so we want to give up on a stalled dial quickly
/// and re-dial the same already-online endpoint rather than wait out a long hang.
const IROH_DIAL_BASE_TIMEOUT: Duration = Duration::from_secs(4);
/// QUIC is already connected here; both application streams should complete
/// their kind ACKs promptly. Expiry returns control to the reconnect loop.
///
/// Generous on purpose: a relay-routed handshake carries the stream opens and
/// both ACKs, and giving up on one that is merely slow costs a full re-dial —
/// which delays the direct-path upgrade that would have made it fast. Kept
/// under `IROH_ONLINE_TIMEOUT` so a genuinely wedged handshake still surfaces.
#[cfg(not(test))]
const IROH_STREAM_READY_TIMEOUT: Duration = Duration::from_secs(10);
/// The timeout tests wait this out; the behaviour is the same at any length.
#[cfg(test)]
const IROH_STREAM_READY_TIMEOUT: Duration = Duration::from_secs(1);
/// How many times to re-dial on the same warm endpoint before surfacing an error
/// to the reconnect loop (which then cold-starts a fresh endpoint). Each attempt
/// gets `BASE + 2s * attempt`, so 4s + 6s + 8s = 18s of dialling on one online().
const IROH_DIAL_ATTEMPTS: usize = 3;
/// Growth per relay-routed dial attempt (see [`DialSchedule`]).
const IROH_DIAL_STEP: Duration = Duration::from_secs(2);
/// Attempts for a dial made with no relay home. Two, not one: a single dropped
/// handshake packet must not be the whole verdict on an address.
const IROH_DIRECT_DIAL_ATTEMPTS: usize = 2;

/// How long to spend dialling, decided by whether the endpoint reached a relay
/// home.
///
/// Pure data with no endpoint and no IO, so the policy is unit-testable without
/// touching the process-shared endpoint pool — whose `static` lifetime makes it
/// hostile to parallel tests.
///
/// Two schedules exist because the two paths fail differently:
///
/// - **Relay-routed** dials get an escalating ladder. The first dial to a peer
///   routinely stalls while the relay primes the cross-path, and the re-dial
///   then connects in milliseconds, so patience genuinely pays here.
/// - **Relay-less** dials go straight at addresses we already hold. Such a dial
///   either completes within an RTT or the address is unreachable; escalating
///   only makes the caller wait longer for a verdict that will not change.
///
/// The relay-less budget is bounded by [`IROH_ONLINE_TIMEOUT`]: before this
/// policy existed, an unreachable relay cost the user exactly that long before
/// failing. Dialling direct addresses instead of giving up must not cost more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DialSchedule {
    attempts: usize,
    base_timeout: Duration,
    step: Duration,
}

/// Error for a dial that ran out of attempts, naming the path that was actually
/// tried. Without the distinction a relay-less failure reads as a plain timeout,
/// hiding the one fact that explains it: there was no relay, so only the direct
/// addresses could have worked, and none did.
fn dial_exhausted_error(relay_online: bool, last_cause: Option<&str>) -> String {
    let class = if relay_online {
        "iroh controller connect timeout"
    } else {
        "iroh controller connect timeout: no relay home, direct addresses unreachable"
    };
    match last_cause.map(str::trim).filter(|cause| !cause.is_empty()) {
        Some(cause) => format!("{class}: {cause}"),
        None => class.to_string(),
    }
}

impl DialSchedule {
    fn for_relay_state(relay_online: bool) -> Self {
        if relay_online {
            Self {
                attempts: IROH_DIAL_ATTEMPTS,
                base_timeout: IROH_DIAL_BASE_TIMEOUT,
                step: IROH_DIAL_STEP,
            }
        } else {
            Self {
                attempts: IROH_DIRECT_DIAL_ATTEMPTS,
                base_timeout: IROH_DIAL_BASE_TIMEOUT,
                step: Duration::ZERO,
            }
        }
    }

    /// Timeout for `attempt` (0-based). Saturates at the last attempt so an
    /// out-of-range index can neither panic nor grow the budget.
    fn timeout_for(&self, attempt: usize) -> Duration {
        let step = attempt.min(self.attempts.saturating_sub(1));
        self.base_timeout + self.step * step as u32
    }

    /// Worst-case wall time this schedule can spend before it gives up. Exists to
    /// make the relay-less budget bound assertable; nothing in the dial path needs
    /// the total, only the per-attempt timeout.
    #[cfg(test)]
    fn total_budget(&self) -> Duration {
        (0..self.attempts)
            .map(|attempt| self.timeout_for(attempt))
            .sum()
    }
}

type IrohSender = mpsc::Sender<Vec<u8>>;
static HOST_PEER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct PeerSenders {
    id: u64,
    connection: Connection,
    control: IrohSender,
    terminal: IrohSender,
    closed: Arc<AtomicBool>,
}

#[derive(Clone)]
struct TerminalUploadReply {
    device_id: String,
    session_id: String,
    request_id: Option<String>,
}

impl From<&RemoteTransportUpload> for TerminalUploadReply {
    fn from(upload: &RemoteTransportUpload) -> Self {
        Self {
            device_id: upload.device_id.clone(),
            session_id: upload.session_id.clone(),
            request_id: upload.request_id.clone(),
        }
    }
}

/// Holds one connection's streams until both outbound queues have real writers.
/// Publishing a peer before this pair is complete lets `send_terminal` report
/// success while its receiver still sits unread in the accept loop.
struct PendingPeerStreams<T> {
    control: Option<T>,
    terminal: Option<T>,
    started: bool,
}

impl<T> PendingPeerStreams<T> {
    fn new() -> Self {
        Self {
            control: None,
            terminal: None,
            started: false,
        }
    }

    fn stage(&mut self, kind: u8, stream: T) -> Result<(), T> {
        let slot = match kind {
            STREAM_KIND_CONTROL => &mut self.control,
            STREAM_KIND_TERMINAL => &mut self.terminal,
            _ => return Err(stream),
        };
        if self.started || slot.is_some() {
            return Err(stream);
        }
        *slot = Some(stream);
        Ok(())
    }

    fn take_ready(&mut self) -> Option<(T, T)> {
        if self.started || self.control.is_none() || self.terminal.is_none() {
            return None;
        }
        self.started = true;
        Some((self.control.take()?, self.terminal.take()?))
    }
}

pub struct RemoteIrohHostTransport {
    endpoint: Endpoint,
    blob_store: MemStore,
    /// Serializes peer replacement with pairing's durable side effect and
    /// response publication. A generation check cannot protect that interval.
    peer_ownership: Mutex<()>,
    peers: Mutex<HashMap<String, PeerSenders>>,
    on_message: RemoteTransportMessageHandler,
    on_upload: RemoteTransportUploadHandler,
    on_state: RemoteTransportStateHandler,
    on_pairing: RemoteTransportPairingHandler,
    on_authorize: RemoteTransportAuthorizationHandler,
    on_web_tunnel_tcp_connect: Option<WebTunnelTcpConnectHandler>,
    on_log: Option<RemoteTransportLogHandler>,
    /// The iroh `Router` driving our accept loop. Stored so it stays alive (it
    /// aborts on drop); set once right after construction.
    router: OnceLock<Router>,
}

/// Adapts the host transport to iroh's `ProtocolHandler` so the `Router` drives
/// incoming `CODUX_REMOTE_ALPN` connections. Holds a `Weak` back-ref so the
/// router (owned by the transport) doesn't keep the transport alive in a cycle.
#[derive(Debug, Clone)]
struct CoduxRemoteProtocol {
    transport: Weak<RemoteIrohHostTransport>,
}

/// Separate ALPN used for host-browser web proxy traffic. Browser requests are
/// intentionally isolated from the control/terminal ALPN so large pages cannot
/// block regular remote-control messages.
#[derive(Debug, Clone)]
struct CoduxWebTunnelProtocol {
    transport: Weak<RemoteIrohHostTransport>,
}

impl ProtocolHandler for CoduxRemoteProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if let Some(transport) = self.transport.upgrade() {
            transport.handle_connection(connection).await;
        }
        Ok(())
    }
}

impl ProtocolHandler for CoduxWebTunnelProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if let Some(transport) = self.transport.upgrade() {
            transport.handle_web_tunnel_connection(connection).await;
        }
        Ok(())
    }
}

impl RemoteIrohHostTransport {
    pub async fn connect(
        config: &RemoteHostTransportConfig,
        handlers: RemoteHostTransportHandlers,
    ) -> Result<Arc<Self>, String> {
        let RemoteHostTransportHandlers {
            on_message,
            on_upload,
            on_state,
            on_pairing,
            on_authorize,
            on_web_tunnel_tcp_connect,
            on_log,
        } = handlers;
        let configured_relay_url = iroh_relay_url(config)?;
        let relay_authentication = config.iroh_relay_authentication.trim().to_string();
        let endpoint = endpoint_builder(
            configured_relay_url.as_ref(),
            &relay_authentication,
            host_secret_key(config).as_ref(),
        )
        .alpns(vec![
            CODUX_REMOTE_ALPN.to_vec(),
            CODUX_WEB_TUNNEL_ALPN.to_vec(),
            iroh_blobs::ALPN.to_vec(),
        ])
        .bind()
        .await
        .map_err(|error| format!("iroh host bind failed: {error}"))?;
        // Advertise on the LAN so co-located controllers reach us directly.
        enable_local_discovery(&endpoint, &on_log);
        // Best-effort, exactly as on the controller side: a relay lets remote
        // controllers find us, but its absence does not make this host
        // unreachable — the endpoint ticket we publish below carries our direct
        // addresses, and a controller on the same LAN or SD-WAN network dials
        // those with no relay involved. Failing startup here would take the host
        // down for every controller, including the ones already one hop away.
        let _ = tokio::time::timeout(IROH_ONLINE_TIMEOUT, endpoint.online()).await;
        // Only the endpoint can say whether the configured relay became a live
        // route. Configuration is intent, not reachability: advertising it as
        // live would let the QR serializer discard the direct-address ticket.
        let live_relay = connected_relay_url(&endpoint);
        let relay_online = live_relay.is_some();
        let live_relay_url = live_relay.map(|url| url.to_string()).unwrap_or_default();
        log_transport(
            &on_log,
            format!("iroh_host online={relay_online} relay={live_relay_url}"),
        );
        let blob_store = new_blob_store();
        let transport = Arc::new(Self {
            endpoint,
            blob_store: blob_store.clone(),
            peer_ownership: Mutex::new(()),
            peers: Mutex::new(HashMap::new()),
            on_message,
            on_upload,
            on_state,
            on_pairing,
            on_authorize,
            on_web_tunnel_tcp_connect,
            on_log,
            router: OnceLock::new(),
        });
        // Run our star protocol as a standard iroh custom protocol: the Router
        // owns the accept loop and dispatches by ALPN (our control/terminal
        // streams vs iroh-blobs), replacing the hand-rolled accept+match. The
        // handler holds a Weak back-ref so transport<->router don't form an Arc
        // cycle; closing the transport shuts the Router down.
        let blob_protocol = BlobsProtocol::new(blob_store.as_ref(), None);
        let router = Router::builder(transport.endpoint.clone())
            .accept(
                CODUX_REMOTE_ALPN,
                CoduxRemoteProtocol {
                    transport: Arc::downgrade(&transport),
                },
            )
            .accept(
                CODUX_WEB_TUNNEL_ALPN,
                CoduxWebTunnelProtocol {
                    transport: Arc::downgrade(&transport),
                },
            )
            .accept(iroh_blobs::ALPN, blob_protocol)
            .spawn();
        let _ = transport.router.set(router);
        Ok(transport)
    }

    async fn handle_connection(self: Arc<Self>, connection: Connection) {
        let peer_key = connection.remote_id().to_string();
        let (control_tx, control_rx) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
        let (terminal_tx, terminal_rx) = mpsc::channel::<Vec<u8>>(TERMINAL_QUEUE_CAPACITY);
        let peer_senders = PeerSenders {
            id: HOST_PEER_ID.fetch_add(1, Ordering::Relaxed),
            connection: connection.clone(),
            control: control_tx,
            terminal: terminal_tx,
            closed: Arc::new(AtomicBool::new(false)),
        };
        let mut control_rx = Some(control_rx);
        let mut terminal_rx = Some(terminal_rx);
        let mut streams = PendingPeerStreams::new();
        let accepted_at = std::time::Instant::now();
        let ready_deadline = tokio::time::Instant::now() + IROH_STREAM_READY_TIMEOUT;
        loop {
            let (mut send, mut recv) =
                match tokio::time::timeout_at(ready_deadline, connection.accept_bi()).await {
                    Ok(Ok(streams)) => streams,
                    Ok(Err(error)) => {
                        self.log(format!(
                            "iroh_host_accept_stream failed peer={peer_key} error={error}"
                        ));
                        break;
                    }
                    Err(_) => {
                        self.log(format!(
                            "iroh_host_stream_ready timeout peer={peer_key} ms={}",
                            accepted_at.elapsed().as_millis()
                        ));
                        connection.close(0u32.into(), b"stream readiness timeout");
                        break;
                    }
                };
            let kind =
                match tokio::time::timeout_at(ready_deadline, read_stream_kind(&mut recv)).await {
                    Ok(Ok(kind)) => kind,
                    Ok(Err(error)) => {
                        self.log(format!(
                            "iroh_host_stream_kind failed peer={peer_key} error={error}"
                        ));
                        continue;
                    }
                    Err(_) => {
                        self.log(format!(
                            "iroh_host_stream_kind timeout peer={peer_key} ms={}",
                            accepted_at.elapsed().as_millis()
                        ));
                        connection.close(0u32.into(), b"stream kind timeout");
                        break;
                    }
                };
            let label = match kind {
                STREAM_KIND_CONTROL => "control",
                STREAM_KIND_TERMINAL => "terminal",
                _ => {
                    self.log(format!(
                        "unsupported iroh stream kind {kind} peer={peer_key}"
                    ));
                    continue;
                }
            };
            match tokio::time::timeout_at(ready_deadline, write_stream_kind(&mut send, kind)).await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.log(format!(
                        "iroh_host_{label}_init failed peer={peer_key} error={error}"
                    ));
                    continue;
                }
                Err(_) => {
                    self.log(format!("iroh_host_{label}_ack timeout peer={peer_key}"));
                    connection.close(0u32.into(), b"stream ack timeout");
                    break;
                }
            }
            if streams.stage(kind, (send, recv)).is_err() {
                self.log(format!(
                    "iroh_host_duplicate_{label}_stream peer={peer_key}"
                ));
                continue;
            }
            let Some(((control_send, control_recv), (terminal_send, terminal_recv))) =
                streams.take_ready()
            else {
                continue;
            };
            let (Some(control_rx), Some(terminal_rx)) = (control_rx.take(), terminal_rx.take())
            else {
                break;
            };

            // Writers own both receivers before the peer becomes reachable.
            // From this point, every successful queue send has a live consumer.
            spawn_writer(control_send, control_rx, self.on_log.clone(), {
                let transport = Arc::clone(&self);
                let peer_key = peer_key.clone();
                let peer_id = peer_senders.id;
                let closed = Arc::clone(&peer_senders.closed);
                let connection = peer_senders.connection.clone();
                move || {
                    transport.close_peer(peer_id, &peer_key, "control_write", &closed, &connection)
                }
            });
            spawn_writer(terminal_send, terminal_rx, self.on_log.clone(), {
                let transport = Arc::clone(&self);
                let peer_key = peer_key.clone();
                let peer_id = peer_senders.id;
                let closed = Arc::clone(&peer_senders.closed);
                let connection = peer_senders.connection.clone();
                move || {
                    transport.close_peer(peer_id, &peer_key, "terminal_write", &closed, &connection)
                }
            });
            if !self.insert_peer(peer_key.clone(), peer_senders.clone()) {
                connection.close(0u32.into(), b"superseded connection");
                break;
            }
            self.log(format!(
                "iroh_host_streams_ready peer={peer_key} ms={}",
                accepted_at.elapsed().as_millis()
            ));
            (self.on_state)(peer_key.clone(), "connected".to_string());
            self.spawn_peer_path_logger(connection.clone(), peer_key.clone());
            self.spawn_peer_reader(
                control_recv,
                peer_key.clone(),
                peer_senders.clone(),
                "control",
            );
            self.spawn_peer_reader(
                terminal_recv,
                peer_key.clone(),
                peer_senders.clone(),
                "terminal",
            );
            return;
        }
        self.close_peer(
            peer_senders.id,
            &peer_key,
            "accept_loop",
            &peer_senders.closed,
            &peer_senders.connection,
        );
    }

    async fn handle_web_tunnel_connection(self: Arc<Self>, connection: Connection) {
        let peer_key = connection.remote_id().to_string();
        loop {
            let (send, recv) = match connection.accept_bi().await {
                Ok(streams) => streams,
                Err(error) => {
                    self.log(format!(
                        "iroh_web_tunnel_accept_stream failed peer={peer_key} error={error}"
                    ));
                    break;
                }
            };
            let transport = Arc::clone(&self);
            let peer = peer_key.clone();
            tokio::spawn(async move {
                if let Err(error) = transport.handle_web_tunnel_stream(send, recv).await {
                    transport.log(format!(
                        "iroh_web_tunnel_stream failed peer={peer} error={error}"
                    ));
                }
            });
        }
    }

    async fn handle_web_tunnel_stream(
        &self,
        mut send: SendStream,
        mut recv: RecvStream,
    ) -> Result<(), String> {
        let Some(header) = read_frame(&mut recv).await? else {
            return Err("missing web tunnel request header".to_string());
        };
        let envelope: WebTunnelRequestEnvelope =
            serde_json::from_slice(&header).map_err(|error| error.to_string())?;
        if envelope.kind != WEB_TUNNEL_KIND_TCP_CONNECT {
            return Err("unsupported web tunnel request kind".to_string());
        }
        let target_host = envelope.target_host;
        let target_port = envelope.target_port;
        let Some(handler) = self.on_web_tunnel_tcp_connect.as_ref() else {
            write_web_tunnel_response(
                &mut send,
                WebTunnelResponse::error(503, "web tunnel is not enabled on this host"),
            )
            .await?;
            return Ok(());
        };
        if let Err(error) = handler(WebTunnelTcpConnectRequest {
            device_id: envelope.device_id,
            device_token: envelope.device_token,
            host: target_host.clone(),
            port: target_port,
        }) {
            write_web_tunnel_response(&mut send, WebTunnelResponse::error(401, error)).await?;
            return Ok(());
        }
        let host_stream =
            match tokio::net::TcpStream::connect((target_host.as_str(), target_port)).await {
                Ok(stream) => stream,
                Err(error) => {
                    write_web_tunnel_response(
                        &mut send,
                        WebTunnelResponse::error(502, error.to_string()),
                    )
                    .await?;
                    return Ok(());
                }
            };
        write_web_tunnel_response(
            &mut send,
            WebTunnelResponse {
                status: 200,
                headers: Vec::new(),
                body: Vec::new(),
                error: None,
            },
        )
        .await?;
        let _ = relay_bidirectional(TunnelStream { recv, send }, host_stream)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    /// Log how a peer is actually reached, and again whenever the path changes.
    ///
    /// Without it a "connected but slow" report cannot be told apart from
    /// "never upgraded off the relay": the host knew which path QUIC selected
    /// and said nothing. The controller already reports this to its UI; the
    /// headless host only had a log.
    fn spawn_peer_path_logger(self: &Arc<Self>, connection: Connection, peer_key: String) {
        let transport = Arc::clone(self);
        tokio::spawn(async move {
            transport.log_peer_path(&connection, &peer_key);
            let mut events = connection.path_events();
            while let Some(event) = events.next().await {
                match event {
                    PathEvent::Selected { .. } | PathEvent::Lagged { .. } => {
                        transport.log_peer_path(&connection, &peer_key);
                    }
                    _ => {}
                }
            }
        });
    }

    fn log_peer_path(&self, connection: &Connection, peer_key: &str) {
        for path in connection.paths().iter() {
            if path.is_selected() {
                self.log(format!(
                    "iroh_host_path peer={peer_key} path={} addr={} rtt_ms={}",
                    transport_path_kind(path.remote_addr()),
                    transport_addr_label(path.remote_addr()),
                    path.rtt().as_millis(),
                ));
                return;
            }
        }
        self.log(format!("iroh_host_path peer={peer_key} path=unknown"));
    }

    fn spawn_peer_reader(
        self: &Arc<Self>,
        recv: RecvStream,
        peer_key: String,
        senders: PeerSenders,
        label: &'static str,
    ) {
        let transport = Arc::clone(self);
        tokio::spawn(async move {
            let transport_for_frame = Arc::clone(&transport);
            let peer_key_for_frame = peer_key.clone();
            let senders_for_frame = senders.clone();
            let peer_id = senders.id;
            let closed = Arc::clone(&senders.closed);
            let connection = senders.connection.clone();
            let read_result = read_loop(
                recv,
                peer_key.clone(),
                Arc::new(move |device_id, data| {
                    transport_for_frame.handle_frame(
                        &peer_key_for_frame,
                        &senders_for_frame,
                        device_id,
                        data,
                    );
                }),
            )
            .await;
            if let Err(error) = read_result {
                transport.log(format!(
                    "iroh_host_{label}_read failed peer={} error={error}",
                    peer_key
                ));
            }
            transport.close_peer(peer_id, &peer_key, label, &closed, &connection);
        });
    }

    fn handle_frame(
        self: &Arc<Self>,
        peer_key: &str,
        senders: &PeerSenders,
        fallback_device_id: String,
        data: Vec<u8>,
    ) {
        if senders.closed.load(Ordering::SeqCst) || !self.peer_is_current(peer_key, senders.id) {
            // End the connection rather than dropping frame after frame: a
            // client whose messages vanish has nothing to react to.
            senders.connection.close(0u32.into(), b"stale connection");
            self.log(format!(
                "iroh_host_recv drop peer={peer_key} reason=stale_connection"
            ));
            return;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) else {
            self.log("iroh_host_recv drop reason=decode".to_string());
            return;
        };
        // `deviceToken` is the only field the envelope struct does not carry, so
        // lift it out first and then MOVE the tree into `from_value`. Keeping
        // `value` alive afterwards would force a deep clone of the whole frame
        // (payloads reach 16 KB) on every inbound message from every peer.
        let device_token = value
            .get("deviceToken")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let Ok(raw) = serde_json::from_value::<RemoteEnvelope>(value) else {
            self.log("iroh_host_recv drop reason=envelope".to_string());
            return;
        };
        let device_id = raw
            .device_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(fallback_device_id);
        if raw.kind == codux_protocol::REMOTE_PAIRING_REQUEST {
            let response_permit = match senders.control.try_reserve() {
                Ok(permit) => permit,
                Err(error) => {
                    self.log(format!(
                        "iroh_host_recv drop peer={peer_key} reason=pairing_response_unavailable error={error}"
                    ));
                    senders
                        .connection
                        .close(0u32.into(), b"pairing response unavailable");
                    return;
                }
            };
            let Ok(_ownership) = self.peer_ownership.lock() else {
                self.log(format!(
                    "iroh_host_recv drop peer={peer_key} reason=peer_ownership_unavailable"
                ));
                return;
            };
            if senders.closed.load(Ordering::SeqCst) || !self.peer_is_current(peer_key, senders.id)
            {
                self.log(format!(
                    "iroh_host_recv drop peer={peer_key} reason=stale_pairing_connection"
                ));
                return;
            }
            let handshake = crate::control_messages::pairing_handshake_from_envelope(&raw);
            let pairing_device_id = handshake
                .as_ref()
                .map(|handshake| handshake.device_id.clone());
            let candidate_device_id = pairing_device_id.as_deref().unwrap_or(&device_id);
            if !candidate_device_id.trim().is_empty()
                && !self.peer_can_bind_alias_under_ownership(peer_key, candidate_device_id, senders)
            {
                self.log(format!(
                    "iroh_host_recv drop peer={peer_key} reason=stale_pairing_connection"
                ));
                return;
            }
            let response_payload = handshake.and_then(|handshake| (self.on_pairing)(handshake));
            let Some(response_payload) = response_payload else {
                if let Some(response) = transport_rejection_data(&raw, true) {
                    response_permit.send(response);
                }
                return;
            };
            let pairing_device_id = pairing_device_id.unwrap_or(device_id);
            if !pairing_device_id.trim().is_empty() {
                if !self.bind_peer_alias_under_ownership(peer_key, &pairing_device_id, senders) {
                    self.log(format!(
                        "iroh_host_recv drop peer={peer_key} reason=stale_pairing_connection"
                    ));
                    return;
                }
                (self.on_state)(pairing_device_id.clone(), "connected".to_string());
            }
            let mut response = serde_json::json!({
                "type": codux_protocol::REMOTE_PAIRING_CONFIRMED,
                "deviceId": pairing_device_id,
                "payload": response_payload,
            });
            if let Some(request_id) = raw.request_id.as_deref() {
                response["requestId"] = serde_json::Value::String(request_id.to_string());
            }
            if let Ok(response) = serde_json::to_vec(&response) {
                response_permit.send(response);
            }
            return;
        }
        let device_token = device_token.as_deref().unwrap_or_default();
        if !(self.on_authorize)(&device_id, device_token) {
            self.send_transport_rejection(senders, &raw, false);
            return;
        }
        if !device_id.trim().is_empty() {
            if !self.bind_peer_alias(peer_key, &device_id, senders) {
                self.log(format!(
                    "iroh_host_recv drop peer={peer_key} reason=stale_authorized_connection"
                ));
                return;
            }
            (self.on_state)(device_id.clone(), "connected".to_string());
        }
        if let Some(pong) = crate::control_messages::transport_pong_for_ping(&raw, Some(&device_id))
        {
            let _ = self.send(pong.into_bytes(), Some(&device_id));
            return;
        }
        if raw.kind == REMOTE_TERMINAL_UPLOAD_BLOB {
            let reply = TerminalUploadReply {
                device_id: device_id.clone(),
                session_id: raw.session_id.clone().unwrap_or_default(),
                request_id: raw
                    .request_id
                    .clone()
                    .filter(|request_id| !request_id.trim().is_empty()),
            };
            let upload = match upload_metadata_from_blob_envelope(&raw, &device_id) {
                Ok(upload) => upload,
                Err(error) => {
                    self.log(format!("iroh_host_blob_upload rejected error={error}"));
                    let on_log = self.on_log.clone();
                    let transport = Arc::clone(self);
                    tokio::spawn(async move {
                        publish_terminal_upload_reply(transport, reply, Err(error), on_log).await;
                    });
                    return;
                }
            };
            let endpoint = self.endpoint.clone();
            let blob_store = self.blob_store.clone();
            let on_upload = Arc::clone(&self.on_upload);
            let on_log = self.on_log.clone();
            let transport = Arc::clone(self);
            tokio::spawn(async move {
                let result =
                    match download_terminal_upload_blob(&blob_store, &endpoint, upload).await {
                        Ok(upload) => on_upload(upload),
                        Err(error) => Err(error),
                    };
                publish_terminal_upload_reply(transport, reply, result, on_log).await;
            });
            return;
        }
        (self.on_message)(device_id, data);
    }

    fn send_transport_rejection(
        &self,
        senders: &PeerSenders,
        request: &RemoteEnvelope,
        pairing: bool,
    ) {
        if let Some(data) = transport_rejection_data(request, pairing) {
            let _ = senders.control.try_send(data);
        }
    }

    fn insert_peer(&self, peer_key: String, senders: PeerSenders) -> bool {
        let Ok(_ownership) = self.peer_ownership.lock() else {
            return false;
        };
        let replaced = {
            let Ok(mut peers) = self.peers.lock() else {
                return false;
            };
            let current = peers.get(&peer_key).cloned();
            if current
                .as_ref()
                .is_some_and(|current| current.id > senders.id)
            {
                senders.closed.store(true, Ordering::SeqCst);
                senders
                    .connection
                    .close(0u32.into(), b"superseded connection");
                return false;
            }
            let replaced = current.filter(|current| current.id != senders.id);
            let aliases = replaced.as_ref().map(|current| {
                current.closed.store(true, Ordering::SeqCst);
                remove_peer_aliases_by_id(&mut peers, current.id)
            });
            peers.insert(peer_key, senders.clone());
            replaced.zip(aliases)
        };
        if let Some((replaced, aliases)) = replaced {
            self.notify_replaced_peer(replaced, senders.id, aliases);
        }
        true
    }

    fn bind_peer_alias(&self, peer_key: &str, device_id: &str, senders: &PeerSenders) -> bool {
        let Ok(_ownership) = self.peer_ownership.lock() else {
            return false;
        };
        self.bind_peer_alias_under_ownership(peer_key, device_id, senders)
    }

    fn peer_can_bind_alias_under_ownership(
        &self,
        peer_key: &str,
        device_id: &str,
        senders: &PeerSenders,
    ) -> bool {
        let Ok(peers) = self.peers.lock() else {
            return false;
        };
        !senders.closed.load(Ordering::SeqCst)
            && peers.get(peer_key).map(|peer| peer.id) == Some(senders.id)
            && !peers
                .get(device_id)
                .is_some_and(|current| current.id > senders.id)
    }

    /// Caller holds `peer_ownership`, so the current-owner check, alias commit,
    /// and any surrounding side effect form one transaction.
    fn bind_peer_alias_under_ownership(
        &self,
        peer_key: &str,
        device_id: &str,
        senders: &PeerSenders,
    ) -> bool {
        let replaced = {
            let Ok(mut peers) = self.peers.lock() else {
                return false;
            };
            if senders.closed.load(Ordering::SeqCst)
                || peers.get(peer_key).map(|peer| peer.id) != Some(senders.id)
            {
                return false;
            }
            let current = peers.get(device_id).cloned();
            if current
                .as_ref()
                .is_some_and(|current| current.id > senders.id)
            {
                senders.closed.store(true, Ordering::SeqCst);
                let aliases = remove_peer_aliases_by_id(&mut peers, senders.id);
                drop(peers);
                senders
                    .connection
                    .close(0u32.into(), b"superseded connection");
                for alias in aliases {
                    (self.on_state)(alias, "closed".to_string());
                }
                return false;
            }
            let replaced = current.filter(|current| current.id != senders.id);
            let aliases = replaced.as_ref().map(|current| {
                current.closed.store(true, Ordering::SeqCst);
                remove_peer_aliases_by_id(&mut peers, current.id)
            });
            peers.insert(device_id.to_string(), senders.clone());
            replaced.zip(aliases)
        };
        if let Some((replaced, aliases)) = replaced {
            self.notify_replaced_peer(replaced, senders.id, aliases);
        }
        true
    }

    fn peer_is_current(&self, peer_key: &str, peer_id: u64) -> bool {
        self.peers
            .lock()
            .ok()
            .and_then(|peers| peers.get(peer_key).map(|peer| peer.id))
            == Some(peer_id)
    }

    fn notify_replaced_peer(&self, replaced: PeerSenders, new_id: u64, aliases: Vec<String>) {
        replaced
            .connection
            .close(0u32.into(), b"superseded connection");
        let old_id = replaced.id;
        self.log(format!(
            "iroh_host_peer_replaced old_id={old_id} new_id={new_id}"
        ));
        for alias in aliases {
            (self.on_state)(alias, "closed".to_string());
        }
    }

    fn remove_peer_aliases_by_id(&self, peer_id: u64) -> Vec<String> {
        let Ok(mut peers) = self.peers.lock() else {
            return Vec::new();
        };
        remove_peer_aliases_by_id(&mut peers, peer_id)
    }

    /// Retire a peer: one stream ending takes the whole connection with it.
    ///
    /// The two streams share one `closed` flag and one alias set, so a peer
    /// whose connection stays open after this is half dead — the surviving
    /// reader can only drop what it receives, and the client keeps talking into
    /// a host that answers nothing. Closing the connection here is what tells
    /// it to reconnect instead.
    fn close_peer(
        &self,
        peer_id: u64,
        peer_key: &str,
        reason: &str,
        closed: &Arc<AtomicBool>,
        connection: &Connection,
    ) {
        let Ok(_ownership) = self.peer_ownership.lock() else {
            connection.close(0u32.into(), b"peer ownership unavailable");
            return;
        };
        if closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let aliases = self.remove_peer_aliases_by_id(peer_id);
        connection.close(0u32.into(), b"peer closed");
        self.log(format!(
            "iroh_host_peer_closed peer={peer_key} reason={reason}"
        ));
        for alias in aliases {
            (self.on_state)(alias, "closed".to_string());
        }
    }

    /// Resolve a device's sender at the moment of sending. A device alias is
    /// rebound whenever the phone reconnects, so anything that captured a
    /// sender earlier is addressing a connection that may already be retired.
    fn peer_sender_for(
        &self,
        device_id: &str,
        select: impl Fn(&PeerSenders) -> &IrohSender,
    ) -> Option<IrohSender> {
        let peers = self.peers.lock().ok()?;
        peers
            .get(device_id)
            .filter(|peer| !peer.closed.load(Ordering::SeqCst))
            .map(|peer| select(peer).clone())
    }

    fn send_to_peers(
        &self,
        data: Vec<u8>,
        device_id: Option<&str>,
        select: impl Fn(&PeerSenders) -> &IrohSender,
    ) -> bool {
        if let Some(device_id) = device_id.filter(|value| !value.trim().is_empty()) {
            return self
                .peer_sender_for(device_id, select)
                .map(|tx| tx.try_send(data).is_ok())
                .unwrap_or(false);
        }
        let Ok(peers) = self.peers.lock() else {
            return false;
        };
        let mut sent = false;
        let unique = unique_senders(
            peers
                .values()
                .filter(|peer| !peer.closed.load(Ordering::SeqCst))
                .map(select),
        );
        for tx in unique {
            sent |= tx.try_send(data.clone()).is_ok();
        }
        sent
    }

    fn log(&self, message: String) {
        if let Some(on_log) = self.on_log.as_ref() {
            on_log(message);
        }
    }
}

fn transport_rejection_data(request: &RemoteEnvelope, pairing: bool) -> Option<Vec<u8>> {
    let (kind, payload) = if pairing {
        (
            codux_protocol::REMOTE_PAIRING_REJECTED,
            serde_json::json!({ "reason": "pairing_invalid" }),
        )
    } else {
        (
            codux_protocol::REMOTE_ERROR,
            serde_json::json!({
                "code": "device_unauthorized",
                "message": "This device is no longer authorized. Please pair it again."
            }),
        )
    };
    let mut envelope = serde_json::json!({
        "type": kind,
        "payload": payload,
    });
    if let Some(device_id) = request.device_id.as_deref() {
        envelope["deviceId"] = serde_json::json!(device_id);
    }
    if let Some(request_id) = request.request_id.as_deref() {
        envelope["requestId"] = serde_json::json!(request_id);
    }
    serde_json::to_vec(&envelope).ok()
}

fn terminal_upload_reply_data(
    reply: &TerminalUploadReply,
    result: &Result<RemoteTransportUploadOutcome, String>,
) -> Option<Vec<u8>> {
    let (kind, payload) = match result {
        Ok(outcome) => (
            codux_protocol::REMOTE_TERMINAL_UPLOADED,
            serde_json::json!({
                "path": outcome.path,
                "name": outcome.name,
                "kind": outcome.kind,
                "mode": "path",
                "tool": serde_json::Value::Null,
                "inserted": true,
            }),
        ),
        Err(error) => (
            codux_protocol::REMOTE_ERROR,
            serde_json::json!({
                "code": "terminal_upload_failed",
                "message": error,
            }),
        ),
    };
    let mut envelope = serde_json::json!({
        "type": kind,
        "deviceId": reply.device_id,
        "sessionId": reply.session_id,
        "payload": payload,
    });
    if let Some(request_id) = reply.request_id.as_deref() {
        envelope["requestId"] = serde_json::json!(request_id);
    }
    serde_json::to_vec(&envelope).ok()
}

/// Publishes to whichever connection owns the device alias **now**, not to the
/// one the upload arrived on. A phone that drops mid-transfer reconnects with
/// fresh senders while the host is still writing the file, and the file write
/// plus PTY injection are not idempotent: a reply lost to a retired connection
/// makes the phone time out, retry, and get a second file and a second inserted
/// path. Correlation is handled by `requestId`, which is what makes resolving
/// the target late safe.
async fn publish_terminal_upload_reply(
    transport: Arc<RemoteIrohHostTransport>,
    reply: TerminalUploadReply,
    result: Result<RemoteTransportUploadOutcome, String>,
    on_log: Option<RemoteTransportLogHandler>,
) {
    if let Err(error) = result.as_ref()
        && let Some(on_log) = on_log.as_ref()
    {
        on_log(format!("iroh_host_blob_upload failed error={error}"));
    }
    let Some(data) = terminal_upload_reply_data(&reply, &result) else {
        if let Some(on_log) = on_log.as_ref() {
            on_log("iroh_host_blob_upload reply_encode_failed".to_string());
        }
        return;
    };
    let Some(sender) = transport.peer_sender_for(&reply.device_id, |peer| &peer.control) else {
        if let Some(on_log) = on_log.as_ref() {
            on_log(format!(
                "iroh_host_blob_upload reply_no_peer device={} session={}",
                reply.device_id, reply.session_id
            ));
        }
        return;
    };
    if let Err(error) = sender.send(data).await
        && let Some(on_log) = on_log.as_ref()
    {
        on_log(format!(
            "iroh_host_blob_upload reply_failed device={} session={} error={error}",
            reply.device_id, reply.session_id
        ));
    }
}

pub(crate) fn unique_senders<'a>(
    senders: impl IntoIterator<Item = &'a IrohSender>,
) -> Vec<IrohSender> {
    let mut unique = Vec::<IrohSender>::new();
    for tx in senders {
        if unique.iter().any(|current| current.same_channel(tx)) {
            continue;
        }
        unique.push(tx.clone());
    }
    unique
}

fn remove_peer_aliases_by_id(
    peers: &mut HashMap<String, PeerSenders>,
    peer_id: u64,
) -> Vec<String> {
    let aliases = peers
        .iter()
        .filter(|(_, current)| current.id == peer_id)
        .map(|(alias, _)| alias.clone())
        .collect::<Vec<_>>();
    peers.retain(|_, current| current.id != peer_id);
    aliases
}

#[async_trait]
impl RemoteTransport for RemoteIrohHostTransport {
    fn kind(&self) -> RemoteTransportKind {
        RemoteTransportKind::Iroh
    }

    fn send(&self, data: Vec<u8>, device_id: Option<&str>) -> bool {
        self.send_to_peers(data, device_id, |peer| &peer.control)
    }

    fn send_terminal(&self, data: Vec<u8>, device_id: Option<&str>) -> bool {
        self.send_to_peers(data, device_id, |peer| &peer.terminal)
    }

    fn current_dial_candidate(&self) -> Option<RemoteTransportCandidate> {
        // `EndpointAddr` includes a known home relay while it is still
        // connecting. Keep that address in the full ticket, but publish a
        // top-level relay only when the relay-status owner says it is live.
        let addr = self.endpoint.addr();
        let relay_url = connected_relay_url(&self.endpoint)
            .map(|url| url.to_string())
            .unwrap_or_default();
        Some(RemoteTransportCandidate {
            kind: codux_protocol::REMOTE_TRANSPORT_IROH.to_string(),
            url: relay_url.clone(),
            node_id: addr.id.to_string(),
            relay_url,
            ticket: EndpointTicket::from(addr).to_string(),
            relay_authentication: String::new(),
        })
    }

    async fn publish_blob(&self, bytes: Vec<u8>) -> Result<String, String> {
        publish_blob_bytes(&self.blob_store, &self.endpoint, bytes).await
    }

    async fn fetch_blob(&self, ticket: &str) -> Result<Vec<u8>, String> {
        fetch_blob_bytes(&self.blob_store, &self.endpoint, ticket).await
    }

    async fn shutdown(&self) {
        if let Ok(_ownership) = self.peer_ownership.lock()
            && let Ok(mut peers) = self.peers.lock()
        {
            for peer in peers.values() {
                peer.closed.store(true, Ordering::SeqCst);
            }
            peers.clear();
        }
        if let Some(router) = self.router.get() {
            let _ = router.shutdown().await;
        }
        self.endpoint.close().await;
    }
}

/// One controller endpoint, shared across every controller connection in the
/// process — pairing, the control link, and reconnects. The first connection
/// pays bind + `online()` (relay registration, ~3s) AND primes the relay's route
/// to the host (the costly part: the first dial to a peer stalls while the relay
/// establishes that route, the next one connects in ~milliseconds). Pooling lets
/// every later connection skip both, so the control link comes up instantly right
/// after pairing instead of cold-starting a fresh endpoint. Keyed by relay so a
/// relay change rebuilds; evicted when a dial keeps failing on it so a wedged
/// endpoint can't poison every retry.
struct SharedControllerEndpoint {
    relay_key: String,
    endpoint: Endpoint,
    // One blob store + Router shared by every controller transport on this
    // pooled endpoint: a single accept loop (the Router) serves iroh-blobs for
    // all of them from one store, so an incoming fetch always hits the store the
    // blob was published to. (The old per-transport accept loops raced on the
    // shared endpoint and could serve from the wrong store.)
    //
    // The Router is shared rather than owned: dropping it aborts the accept
    // loop, and this entry is retired for reasons that say nothing about the
    // connections running on it — a relay change, or one connect exhausting its
    // dials. Retiring the entry used to kill blob serving for every live
    // transport, so uploads and file writes failed on a link that was otherwise
    // healthy. Now the pool holds one reference and each transport holds
    // another; the loop stops when the last of them is gone.
    blob_store: MemStore,
    router: Arc<Router>,
    // Identity of THIS pooled build. A connect that fails its dials carries the
    // generation it dialed on and only retires the pool entry when it still
    // matches — so a late evict from an ABANDONED reconnect (the Dart layer gave
    // up and started a newer connect) can't clobber the build a concurrent
    // connect already reused or swapped in.
    generation: u64,
}

/// One acquisition of the pooled controller endpoint. Named rather than a tuple
/// so the generation — which decides whether a later evict is allowed to retire
/// this build — can't be read out of position.
///
/// Deliberately carries NO relay flag. Relay state is read live off the endpoint
/// at dial time (see [`connected_relay_url`]); caching it here would let one bad
/// start pin the schedule forever.
struct ControllerEndpoint {
    endpoint: Endpoint,
    blob_store: MemStore,
    generation: u64,
    /// The blob accept loop, kept alive by whoever holds this. A transport must
    /// retain it for as long as it serves blobs — the pool's own reference can
    /// be retired at any time and for reasons unrelated to this connection.
    router: Arc<Router>,
}

/// Fold addresses this controller has previously reached the host on into the
/// dial set its ticket provides.
///
/// The ticket is a snapshot taken at pairing time and never refreshed, so a host
/// that has since moved — new network, new lease, a virtual interface that came
/// up later — is unreachable through it alone. An address that carried a working
/// connection is the cheapest evidence available that the host may still answer
/// there, and adding it costs nothing: `EndpointAddr` holds a set, so an address
/// the ticket already lists merges away instead of being dialled twice.
///
/// Unparseable entries are dropped rather than failing the dial. Stored data
/// outlives the code that wrote it, and one bad row must not cost the user a
/// connection the ticket alone could have made.
fn with_remembered_addrs(addr: EndpointAddr, remembered: &[String]) -> EndpointAddr {
    let parsed = remembered
        .iter()
        .filter_map(|value| value.trim().parse::<SocketAddr>().ok())
        .map(TransportAddr::Ip);
    addr.with_addrs(parsed)
}

/// Return one relay route that `endpoint` is currently connected to.
///
/// Read live on every dial rather than captured when the endpoint was built.
/// iroh keeps retrying the relay in the background, so an endpoint that came up
/// relay-less can gain a home minutes later — and the pooled endpoint is reused
/// for every subsequent connection. A flag captured at build time would keep
/// dialling the relay-less schedule long after the relay came back, and would
/// disagree with the endpoint it claims to describe.
fn connected_relay_url(endpoint: &Endpoint) -> Option<RelayUrl> {
    select_connected_relay_url(
        endpoint
            .home_relay_status()
            .get()
            .into_iter()
            .map(|status| (status.url().clone(), status.is_connected())),
    )
}

fn select_connected_relay_url(
    statuses: impl IntoIterator<Item = (RelayUrl, bool)>,
) -> Option<RelayUrl> {
    statuses
        .into_iter()
        .find_map(|(url, is_connected)| is_connected.then_some(url))
}

static SHARED_CONTROLLER_ENDPOINT: OnceLock<AsyncMutex<Option<SharedControllerEndpoint>>> =
    OnceLock::new();
static SHARED_CONTROLLER_ENDPOINT_GENERATION: AtomicU64 = AtomicU64::new(0);

fn controller_endpoint_key(relay_url: Option<&RelayUrl>, relay_authentication: &str) -> String {
    format!(
        "{}|{}",
        relay_url.map(|url| url.to_string()).unwrap_or_default(),
        relay_authentication.trim()
    )
}

async fn acquire_controller_endpoint(
    relay_url: Option<&RelayUrl>,
    relay_authentication: &str,
    on_log: &Option<RemoteTransportLogHandler>,
) -> Result<ControllerEndpoint, String> {
    let key = controller_endpoint_key(relay_url, relay_authentication);
    let mut guard = SHARED_CONTROLLER_ENDPOINT
        .get_or_init(|| AsyncMutex::new(None))
        .lock()
        .await;
    if let Some(shared) = guard.as_ref() {
        if shared.relay_key == key {
            log_transport(on_log, "iroh_controller endpoint reused".to_string());
            return Ok(ControllerEndpoint {
                endpoint: shared.endpoint.clone(),
                blob_store: shared.blob_store.clone(),
                generation: shared.generation,
                router: Arc::clone(&shared.router),
            });
        }
        // Relay changed under us — drop the old entry and build for the new one.
        // Don't close(): a concurrent connect may still hold a clone of it, and
        // closing a shared handle would wedge that connect mid-dial. It's
        // reclaimed when the last clone drops.
        *guard = None;
    }
    let online_started = std::time::Instant::now();
    let endpoint = endpoint_builder(relay_url, relay_authentication, None)
        .alpns(vec![
            CODUX_WEB_TUNNEL_ALPN.to_vec(),
            iroh_blobs::ALPN.to_vec(),
        ])
        .bind()
        .await
        .map_err(|error| format!("iroh controller bind failed: {error}"))?;
    // Discover co-located hosts directly on the LAN, not just via the relay.
    enable_local_discovery(&endpoint, on_log);
    // Give the endpoint a chance to register with its relay, but do NOT gate the
    // connection on it. `online()` reports whether a relay home exists; it is not
    // a precondition for dialling. A peer's `EndpointAddr` is an address LIST —
    // relay URLs AND direct addresses — and `connect` establishes a direct path
    // from those addresses with no relay involved. Gating here used to strand
    // every controller whose relay was unreachable (a captive portal, a VPN or
    // SD-WAN app owning the default route) even when the host sat one hop away on
    // the same LAN, because the dial loop below never ran.
    //
    // Never `close()` on timeout either: that endpoint is the only object still
    // able to reach the host directly, and iroh keeps retrying the relay on it in
    // the background — so a relay that recovers later brings this very endpoint
    // online without a rebuild.
    let relay_online = tokio::time::timeout(IROH_ONLINE_TIMEOUT, endpoint.online())
        .await
        .is_ok();
    log_transport(
        on_log,
        format!(
            "iroh_controller online={relay_online} elapsed_ms={}",
            online_started.elapsed().as_millis()
        ),
    );
    // `relay_online` above records only how this build STARTED, for the log. The
    // dial schedule reads the endpoint live instead — see `connected_relay_url`.
    // One blob store + Router for every transport that shares this endpoint, so
    // iroh-blobs is served by a single accept loop instead of per-transport
    // loops racing on the shared endpoint.
    let blob_store = new_blob_store();
    let router = Router::builder(endpoint.clone())
        .accept(
            iroh_blobs::ALPN,
            BlobsProtocol::new(blob_store.as_ref(), None),
        )
        .spawn();
    let generation = SHARED_CONTROLLER_ENDPOINT_GENERATION.fetch_add(1, Ordering::Relaxed);
    let router = Arc::new(router);
    *guard = Some(SharedControllerEndpoint {
        relay_key: key,
        endpoint: endpoint.clone(),
        blob_store: blob_store.clone(),
        router: Arc::clone(&router),
        generation,
    });
    Ok(ControllerEndpoint {
        endpoint,
        blob_store,
        generation,
        router,
    })
}

/// Retire the pooled endpoint so the next connection rebuilds a fresh one — used
/// when every dial on it failed (its relay home may have gone stale).
///
/// Two guards keep this safe under overlapping reconnects (the Dart layer can
/// abandon a slow connect and start a newer one while the old one is still
/// dialing the SAME pooled endpoint):
/// - Only retire the entry when it still matches `generation`. A late evict from
///   the abandoned connect must not clobber a build a newer connect already
///   reused or swapped in.
/// - Do NOT `close()` the endpoint. `iroh::Endpoint` is a shared handle, so
///   closing tears down the magicsock for EVERY clone — including the newer
///   connect that reused this same instance, which then fails its in-flight dial
///   with "Internal consistency error". Dropping the pool's reference is enough;
///   the OS resources are reclaimed once the last clone drops. Live transports
///   retain the pooled Router, so blob serving outlives pool-entry retirement.
async fn evict_controller_endpoint(generation: u64) {
    if let Some(cell) = SHARED_CONTROLLER_ENDPOINT.get() {
        let mut guard = cell.lock().await;
        if let Some(shared) = guard.as_ref()
            && shared.generation == generation
        {
            *guard = None;
        }
    }
}

pub struct RemoteIrohControllerTransport {
    endpoint: Endpoint,
    blob_store: MemStore,
    connection: Mutex<Option<Connection>>,
    tx: Mutex<Option<IrohSender>>,
    terminal_tx: Mutex<Option<IrohSender>>,
    upload_tx: Mutex<Option<mpsc::Sender<RemoteTransportUpload>>>,
    closed: Arc<AtomicBool>,
    credentials: Arc<Mutex<ControllerCredentials>>,
    /// The pooled blob accept loop, held for this connection's lifetime.
    ///
    /// Not read — its only job is to exist. The pool retires its own reference
    /// when the relay changes or a connect exhausts its dials, neither of which
    /// says anything about the connections already running on that endpoint;
    /// without this the accept loop stopped underneath them and uploads failed
    /// on an otherwise healthy link.
    _router: Arc<Router>,
}

#[derive(Clone, Default)]
struct ControllerCredentials {
    device_id: String,
    device_token: String,
}

fn select_controller_dial_target(
    config: &RemoteControllerTransportConfig,
) -> Result<(&RemoteTransportCandidate, IrohDialTarget), String> {
    let mut first_error = None;
    for candidate in &config.transports {
        match build_iroh_dial_target(candidate, &config.direct_addrs) {
            Ok(target) => return Ok((candidate, target)),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }
    Err(first_error.unwrap_or_else(|| "missing usable iroh transport candidate".to_string()))
}

impl RemoteIrohControllerTransport {
    pub async fn connect(
        config: &RemoteControllerTransportConfig,
        on_message: RemoteTransportMessageHandler,
        on_state: RemoteTransportStateHandler,
        on_log: Option<RemoteTransportLogHandler>,
    ) -> Result<Arc<Self>, String> {
        let (candidate, target) = select_controller_dial_target(config)?;
        let endpoint_addr = target.endpoint_addr;
        let relay_url = target.controller_relay_url;
        let relay_authentication = candidate.relay_authentication.trim().to_string();
        on_state(String::new(), "connecting".to_string());
        // Reuse the process-shared, already-online controller endpoint: the first
        // connection warms it (relay registration + route priming), every later
        // one — the control link after pairing, reconnects — skips straight to the
        // dial on the warmed path.
        let ControllerEndpoint {
            endpoint,
            blob_store,
            generation: endpoint_generation,
            router,
        } = acquire_controller_endpoint(relay_url.as_ref(), &relay_authentication, &on_log).await?;
        // Read the relay state live off the endpoint, not from how it was built:
        // this endpoint is pooled and reused, and iroh may have acquired a relay
        // home for it since. See `connected_relay_url`.
        let relay_online = connected_relay_url(&endpoint).is_some();
        // Dial with a short timeout and retry on the SAME endpoint. A relay-routed
        // first dial routinely stalls while the relay primes the cross-path (the
        // next dial then connects in ~milliseconds); re-dialling here keeps the
        // relay-registration cost (online() above) paid once, instead of letting
        // the Dart/reconnect layer tear the endpoint down and cold-start a fresh
        // one — which pays online() all over again — for every attempt. Only after
        // exhausting these does the reconnect loop get an error and fall back to a
        // fresh endpoint.
        //
        // With no relay home the schedule is shorter and flat: these dials go
        // straight at the direct addresses in the ticket, which answer within an
        // RTT or not at all. See `DialSchedule`.
        let schedule = DialSchedule::for_relay_state(relay_online);
        let mut connection = None;
        let mut last_dial_cause = None;
        for attempt in 0..schedule.attempts {
            let dial_timeout = schedule.timeout_for(attempt);
            let dial_started = std::time::Instant::now();
            match tokio::time::timeout(
                dial_timeout,
                endpoint.connect(endpoint_addr.clone(), CODUX_REMOTE_ALPN),
            )
            .await
            {
                Ok(Ok(established)) => {
                    log_transport(
                        &on_log,
                        format!(
                            "iroh_controller dial elapsed_ms={} attempt={}",
                            dial_started.elapsed().as_millis(),
                            attempt + 1
                        ),
                    );
                    connection = Some(established);
                    break;
                }
                Ok(Err(error)) => {
                    last_dial_cause = Some(format!("connect failed: {error}"));
                }
                Err(_) => {
                    last_dial_cause = None;
                }
            }
            let attempt_error = dial_exhausted_error(relay_online, last_dial_cause.as_deref());
            log_transport(
                &on_log,
                format!(
                    "iroh_controller dial retry attempt={} elapsed_ms={} reason={attempt_error}",
                    attempt + 1,
                    dial_started.elapsed().as_millis()
                ),
            );
        }
        let Some(connection) = connection else {
            // Every dial on the pooled endpoint failed — retire it so the next
            // attempt rebuilds a fresh one rather than re-dialling a wedged path.
            // Generation-guarded + no-close so this late evict can't pull the rug
            // out from a newer reconnect that already reused this endpoint.
            evict_controller_endpoint(endpoint_generation).await;
            return Err(dial_exhausted_error(
                relay_online,
                last_dial_cause.as_deref(),
            ));
        };
        // Time the handshake on every outcome: it is the one span between "QUIC
        // is up" and "the link is usable", and the budget above can only be
        // tuned against measurements of it.
        let handshake_started = std::time::Instant::now();
        let ready = tokio::time::timeout(
            IROH_STREAM_READY_TIMEOUT,
            initialize_controller_streams(&connection),
        )
        .await;
        let handshake_ms = handshake_started.elapsed().as_millis();
        let ((send, recv), (terminal_send, terminal_recv)) = match ready {
            Ok(Ok(streams)) => {
                log_transport(
                    &on_log,
                    format!("iroh_controller_streams_ready ms={handshake_ms}"),
                );
                streams
            }
            Ok(Err(error)) => {
                log_transport(
                    &on_log,
                    format!("iroh_controller_streams_failed ms={handshake_ms} error={error}"),
                );
                connection.close(0u32.into(), b"stream handshake failed");
                return Err(error);
            }
            Err(_) => {
                log_transport(
                    &on_log,
                    format!("iroh_controller_streams_timeout ms={handshake_ms}"),
                );
                connection.close(0u32.into(), b"stream readiness timeout");
                return Err("iroh controller stream readiness timeout".to_string());
            }
        };
        let (tx, rx) = mpsc::channel::<Vec<u8>>(CONTROL_QUEUE_CAPACITY);
        let (terminal_tx, terminal_rx) = mpsc::channel::<Vec<u8>>(TERMINAL_QUEUE_CAPACITY);
        let (upload_tx, upload_rx) = mpsc::channel::<RemoteTransportUpload>(UPLOAD_QUEUE_CAPACITY);
        let upload_control_tx = tx.clone();
        let credentials = Arc::new(Mutex::new(ControllerCredentials {
            device_id: config.device_id.clone(),
            device_token: config.device_token.clone(),
        }));
        let transport = Arc::new(Self {
            endpoint,
            blob_store: blob_store.clone(),
            connection: Mutex::new(Some(connection.clone())),
            tx: Mutex::new(Some(tx)),
            terminal_tx: Mutex::new(Some(terminal_tx)),
            upload_tx: Mutex::new(Some(upload_tx)),
            closed: Arc::new(AtomicBool::new(false)),
            credentials: Arc::clone(&credentials),
            _router: router,
        });
        // Blobs are served by the pool-level Router (see
        // acquire_controller_endpoint), so there's no per-transport accept loop.
        // The reference above keeps that loop alive for this connection's
        // lifetime: the pool can retire its own at any time, for reasons that
        // say nothing about whether this link is still serving blobs.
        spawn_upload_writer(
            blob_store,
            transport.endpoint.clone(),
            upload_rx,
            upload_control_tx,
            credentials,
            Arc::clone(&on_message),
            on_log.clone(),
        );
        let close_controller: Arc<dyn Fn() + Send + Sync + 'static> = {
            let transport = Arc::clone(&transport);
            let on_state = Arc::clone(&on_state);
            Arc::new(move || {
                if transport.close_sender() {
                    on_state(String::new(), "closed".to_string());
                }
            })
        };
        spawn_terminal_stream(
            terminal_send,
            terminal_recv,
            terminal_rx,
            Arc::clone(&on_message),
            on_log.clone(),
            Arc::clone(&close_controller),
        );
        spawn_control_stream(send, recv, rx, on_message, on_log.clone(), close_controller);
        publish_path_state(&connection, &on_state);
        spawn_path_watcher(connection.clone(), Arc::clone(&on_state));
        Ok(transport)
    }

    fn close_sender(&self) -> bool {
        if self.closed.swap(true, Ordering::SeqCst) {
            return false;
        }
        if let Ok(mut tx) = self.tx.lock() {
            *tx = None;
        }
        if let Ok(mut tx) = self.terminal_tx.lock() {
            *tx = None;
        }
        if let Ok(mut tx) = self.upload_tx.lock() {
            *tx = None;
        }
        if let Ok(mut connection) = self.connection.lock() {
            *connection = None;
        }
        true
    }

    async fn open_web_tunnel_connection(&self) -> Result<Connection, String> {
        let Some(remote_id) =
            self.connection.lock().ok().and_then(|connection| {
                connection.as_ref().map(|connection| connection.remote_id())
            })
        else {
            return Err("iroh controller is not connected".to_string());
        };
        self.endpoint
            .connect(remote_id, CODUX_WEB_TUNNEL_ALPN)
            .await
            .map_err(|error| format!("iroh web tunnel connect failed: {error}"))
    }
}

#[async_trait]
impl RemoteTransport for RemoteIrohControllerTransport {
    fn kind(&self) -> RemoteTransportKind {
        RemoteTransportKind::Iroh
    }

    fn send(&self, data: Vec<u8>, _device_id: Option<&str>) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let Some(data) = authenticated_controller_envelope(data, &self.credentials) else {
            return false;
        };
        self.tx
            .lock()
            .ok()
            .and_then(|tx| tx.clone())
            .map(|tx| tx.try_send(data).is_ok())
            .unwrap_or(false)
    }

    fn send_terminal(&self, data: Vec<u8>, _device_id: Option<&str>) -> bool {
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let Some(data) = authenticated_controller_envelope(data, &self.credentials) else {
            return false;
        };
        self.terminal_tx
            .lock()
            .ok()
            .and_then(|tx| tx.clone())
            .map(|tx| tx.try_send(data).is_ok())
            .unwrap_or(false)
    }

    fn send_terminal_upload(&self, upload: RemoteTransportUpload) -> bool {
        if self.closed.load(Ordering::SeqCst)
            || !upload
                .request_id
                .as_deref()
                .is_some_and(|request_id| !request_id.trim().is_empty())
        {
            return false;
        }
        self.upload_tx
            .lock()
            .ok()
            .and_then(|tx| tx.clone())
            .map(|tx| tx.try_send(upload).is_ok())
            .unwrap_or(false)
    }

    fn set_device_credentials(&self, device_id: &str, device_token: &str) -> bool {
        let Ok(mut credentials) = self.credentials.lock() else {
            return false;
        };
        credentials.device_id = device_id.to_string();
        credentials.device_token = device_token.to_string();
        true
    }

    async fn publish_blob(&self, bytes: Vec<u8>) -> Result<String, String> {
        publish_blob_bytes(&self.blob_store, &self.endpoint, bytes).await
    }

    async fn fetch_blob(&self, ticket: &str) -> Result<Vec<u8>, String> {
        fetch_blob_bytes(&self.blob_store, &self.endpoint, ticket).await
    }

    async fn web_tunnel_tcp_connect(
        &self,
        request: WebTunnelTcpConnectRequest,
    ) -> Result<Box<dyn WebTunnelIoStream>, String> {
        let connection = self.open_web_tunnel_connection().await?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| format!("iroh web tunnel open stream failed: {error}"))?;
        let envelope = WebTunnelRequestEnvelope {
            kind: WEB_TUNNEL_KIND_TCP_CONNECT.to_string(),
            device_id: request.device_id,
            device_token: request.device_token,
            target_host: request.host,
            target_port: request.port,
        };
        let header = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
        write_frame(&mut send, &header).await?;
        let Some(response_header) = read_frame(&mut recv).await? else {
            return Err("missing web tunnel connect response header".to_string());
        };
        let response: WebTunnelResponseEnvelope =
            serde_json::from_slice(&response_header).map_err(|error| error.to_string())?;
        if !(200..300).contains(&response.status) {
            return Err(response
                .error
                .unwrap_or_else(|| format!("web tunnel connect failed: {}", response.status)));
        }
        Ok(Box::new(TunnelStream { recv, send }))
    }

    async fn shutdown(&self) {
        // Do NOT close the endpoint: it is process-shared (see
        // acquire_controller_endpoint), so closing it here would kill every other
        // controller connection and force the next one to cold-start. Dropping our
        // connection (close_sender) is enough; the pool keeps the endpoint warm.
        let _ = self.close_sender();
    }
}

struct TunnelStream {
    recv: RecvStream,
    send: SendStream,
}

impl AsyncRead for TunnelStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for TunnelStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}

async fn relay_bidirectional<A, B>(mut left: A, mut right: B) -> io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    tokio::io::copy_bidirectional(&mut left, &mut right).await
}

fn log_transport(on_log: &Option<RemoteTransportLogHandler>, message: String) {
    if let Some(on_log) = on_log.as_ref() {
        on_log(message);
    }
}

pub(crate) async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), String> {
    if data.len() > MAX_FRAME_BYTES {
        return Err("frame too large".to_string());
    }
    let len = (data.len() as u32).to_be_bytes();
    send.write_all(&len)
        .await
        .map_err(|error| error.to_string())?;
    send.write_all(data)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn write_stream_kind(send: &mut SendStream, kind: u8) -> Result<(), String> {
    send.write_all(&[kind])
        .await
        .map_err(|error| error.to_string())
}

async fn read_stream_kind(recv: &mut RecvStream) -> Result<u8, String> {
    let mut kind = [0u8; 1];
    recv.read_exact(&mut kind)
        .await
        .map_err(|error| error.to_string())?;
    Ok(kind[0])
}

pub(crate) async fn read_frame(recv: &mut RecvStream) -> Result<Option<Vec<u8>>, String> {
    let mut len = [0u8; 4];
    match recv.read_exact(&mut len).await {
        Ok(_) => {}
        Err(error) if error.to_string().contains("closed") => return Ok(None),
        Err(error) if error.to_string().contains("finished") => return Ok(None),
        Err(error) => return Err(error.to_string()),
    }
    let size = u32::from_be_bytes(len) as usize;
    if size > MAX_FRAME_BYTES {
        return Err("frame too large".to_string());
    }
    let mut buf = vec![0u8; size];
    recv.read_exact(&mut buf)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(buf))
}

async fn write_web_tunnel_response(
    send: &mut SendStream,
    response: WebTunnelResponse,
) -> Result<(), String> {
    let envelope = WebTunnelResponseEnvelope {
        status: response.status,
        headers: response
            .headers
            .into_iter()
            .map(WebTunnelHeader::from)
            .collect(),
        body_len: response.body.len() as u64,
        error: response.error,
    };
    let header = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
    write_frame(send, &header).await?;
    if !response.body.is_empty() {
        write_frame(send, &response.body).await?;
    }
    Ok(())
}

async fn read_loop(
    mut recv: RecvStream,
    fallback_device_id: String,
    on_message: RemoteTransportMessageHandler,
) -> Result<(), String> {
    while let Some(data) = read_frame(&mut recv).await? {
        on_message(fallback_device_id.clone(), data);
    }
    Ok(())
}

fn spawn_writer(
    mut send: SendStream,
    mut rx: mpsc::Receiver<Vec<u8>>,
    on_log: Option<RemoteTransportLogHandler>,
    on_close: impl Fn() + Send + 'static,
) {
    tokio::spawn(async move {
        while let Some(data) = rx.recv().await {
            if let Err(error) = write_frame(&mut send, &data).await {
                if let Some(on_log) = on_log.as_ref() {
                    on_log(format!("iroh_write failed error={error}"));
                }
                on_close();
                break;
            }
        }
    });
}

async fn initialize_controller_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    kind: u8,
    label: &'static str,
) -> Result<(SendStream, RecvStream), String> {
    write_stream_kind(&mut send, kind)
        .await
        .map_err(|error| format!("iroh controller {label} init failed: {error}"))?;
    let acknowledged = read_stream_kind(&mut recv)
        .await
        .map_err(|error| format!("iroh controller {label} ACK failed: {error}"))?;
    if acknowledged != kind {
        return Err(format!(
            "iroh controller {label} ACK used unexpected stream kind {acknowledged}"
        ));
    }
    Ok((send, recv))
}

async fn initialize_controller_streams(
    connection: &Connection,
) -> Result<((SendStream, RecvStream), (SendStream, RecvStream)), String> {
    let (send, recv) = connection
        .open_bi()
        .await
        .map_err(|error| format!("iroh controller open stream failed: {error}"))?;
    let (terminal_send, terminal_recv) = connection
        .open_bi()
        .await
        .map_err(|error| format!("iroh controller open terminal stream failed: {error}"))?;
    tokio::try_join!(
        initialize_controller_stream(send, recv, STREAM_KIND_CONTROL, "control"),
        initialize_controller_stream(
            terminal_send,
            terminal_recv,
            STREAM_KIND_TERMINAL,
            "terminal",
        )
    )
}

fn spawn_control_stream(
    send: SendStream,
    recv: RecvStream,
    rx: mpsc::Receiver<Vec<u8>>,
    on_message: RemoteTransportMessageHandler,
    on_log: Option<RemoteTransportLogHandler>,
    on_close: Arc<dyn Fn() + Send + Sync + 'static>,
) {
    tokio::spawn(async move {
        spawn_writer(send, rx, on_log.clone(), {
            let on_close = Arc::clone(&on_close);
            move || on_close()
        });
        let result = read_loop(recv, String::new(), on_message).await;
        if let Err(error) = result
            && let Some(on_log) = on_log.as_ref()
        {
            on_log(format!("iroh_controller_read failed error={error}"));
        }
        on_close();
    });
}

fn spawn_terminal_stream(
    send: SendStream,
    recv: RecvStream,
    rx: mpsc::Receiver<Vec<u8>>,
    on_message: RemoteTransportMessageHandler,
    on_log: Option<RemoteTransportLogHandler>,
    on_close: Arc<dyn Fn() + Send + Sync + 'static>,
) {
    tokio::spawn(async move {
        spawn_writer(send, rx, on_log.clone(), {
            let on_close = Arc::clone(&on_close);
            move || on_close()
        });
        let result = read_loop(recv, String::new(), on_message).await;
        if let Err(error) = result
            && let Some(on_log) = on_log.as_ref()
        {
            on_log(format!(
                "iroh_controller_terminal_read failed error={error}"
            ));
        }
        on_close();
    });
}

fn spawn_upload_writer(
    blob_store: MemStore,
    endpoint: Endpoint,
    mut rx: mpsc::Receiver<RemoteTransportUpload>,
    control_tx: IrohSender,
    credentials: Arc<Mutex<ControllerCredentials>>,
    on_message: RemoteTransportMessageHandler,
    on_log: Option<RemoteTransportLogHandler>,
) {
    tokio::spawn(async move {
        while let Some(upload) = rx.recv().await {
            let reply = TerminalUploadReply::from(&upload);
            if let Err(error) =
                send_terminal_upload_blob(&blob_store, &endpoint, &control_tx, &credentials, upload)
                    .await
            {
                if let Some(on_log) = on_log.as_ref() {
                    on_log(format!("iroh_upload_blob failed error={error}"));
                }
                let result = Err(error);
                let Some(data) = terminal_upload_reply_data(&reply, &result) else {
                    if let Some(on_log) = on_log.as_ref() {
                        on_log("iroh_upload_blob error_encode_failed".to_string());
                    }
                    continue;
                };
                (on_message)(reply.device_id, data);
            }
        }
    });
}

async fn send_terminal_upload_blob(
    blob_store: &MemStore,
    endpoint: &Endpoint,
    control_tx: &IrohSender,
    credentials: &Arc<Mutex<ControllerCredentials>>,
    upload: RemoteTransportUpload,
) -> Result<(), String> {
    let request_id = upload
        .request_id
        .as_deref()
        .filter(|request_id| !request_id.trim().is_empty())
        .ok_or_else(|| "upload request id is required".to_string())?
        .to_string();
    if upload.bytes.is_empty() || upload.bytes.len() > REMOTE_BLOB_MAX_BYTES {
        return Err("upload size is not supported".to_string());
    }
    let total_bytes = upload.bytes.len();
    let ticket = publish_blob_bytes(blob_store, endpoint, upload.bytes).await?;
    let credentials = credentials
        .lock()
        .map(|credentials| credentials.clone())
        .map_err(|_| "controller credentials unavailable".to_string())?;
    let envelope = serde_json::json!({
        "type": REMOTE_TERMINAL_UPLOAD_BLOB,
        "deviceId": credentials.device_id,
        "deviceToken": credentials.device_token,
        "sessionId": upload.session_id,
        "requestId": request_id,
        "payload": {
            "name": upload.name,
            "mime": upload.mime,
            "kind": upload.kind,
            "ticket": ticket,
            "totalBytes": total_bytes,
        },
    });
    let data = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
    control_tx.try_send(data).map_err(|error| error.to_string())
}

fn authenticated_controller_envelope(
    data: Vec<u8>,
    credentials: &Arc<Mutex<ControllerCredentials>>,
) -> Option<Vec<u8>> {
    let credentials = credentials.lock().ok()?.clone();
    let mut envelope = serde_json::from_slice::<serde_json::Value>(&data).ok()?;
    let object = envelope.as_object_mut()?;
    if !credentials.device_id.trim().is_empty() {
        object.insert(
            "deviceId".to_string(),
            serde_json::json!(credentials.device_id),
        );
    }
    if !credentials.device_token.trim().is_empty() {
        object.insert(
            "deviceToken".to_string(),
            serde_json::json!(credentials.device_token),
        );
    }
    serde_json::to_vec(&envelope).ok()
}

fn upload_metadata_from_blob_envelope(
    envelope: &RemoteEnvelope,
    fallback_device_id: &str,
) -> Result<RemoteTransportUpload, String> {
    let payload = &envelope.payload;
    let request_id = envelope
        .request_id
        .clone()
        .filter(|request_id| !request_id.trim().is_empty());
    Ok(RemoteTransportUpload {
        device_id: envelope
            .device_id
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| fallback_device_id.to_string()),
        session_id: envelope.session_id.clone().unwrap_or_default(),
        request_id,
        name: payload
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("upload")
            .to_string(),
        mime: payload
            .get("mime")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        kind: payload
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("file")
            .to_string(),
        bytes: Vec::new(),
        ticket: payload
            .get("ticket")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
    })
}

async fn download_terminal_upload_blob(
    blob_store: &MemStore,
    endpoint: &Endpoint,
    mut upload: RemoteTransportUpload,
) -> Result<RemoteTransportUpload, String> {
    let ticket = upload
        .ticket
        .parse::<BlobTicket>()
        .map_err(|error| error.to_string())?;
    let bytes = fetch_blob_ticket_bytes(blob_store, endpoint, &ticket).await?;
    if bytes.is_empty() {
        return Err("upload size is not supported".to_string());
    }
    upload.bytes = bytes;
    Ok(upload)
}

/// Generic blob publish: add bytes to the store, return a shareable ticket. The
/// terminal upload is a special case of this; the file domain reuses it directly.
async fn publish_blob_bytes(
    blob_store: &MemStore,
    endpoint: &Endpoint,
    bytes: Vec<u8>,
) -> Result<String, String> {
    if bytes.len() > REMOTE_BLOB_MAX_BYTES {
        return Err("blob size is not supported".to_string());
    }
    let content = add_blob_with_lease(blob_store, bytes, BLOB_TICKET_LEASE).await?;
    Ok(BlobTicket::new(endpoint.addr(), content.hash, content.format).to_string())
}

async fn add_blob_with_lease(
    blob_store: &MemStore,
    bytes: Vec<u8>,
    lease: Duration,
) -> Result<HashAndFormat, String> {
    let tag = blob_store
        .add_bytes(bytes)
        .temp_tag()
        .await
        .map_err(|error| error.to_string())?;
    let content = tag.hash_and_format();
    tokio::spawn(async move {
        tokio::time::sleep(lease).await;
        drop(tag);
    });
    Ok(content)
}

/// Generic blob fetch: download the bytes a peer published under `ticket`.
async fn fetch_blob_bytes(
    blob_store: &MemStore,
    endpoint: &Endpoint,
    ticket: &str,
) -> Result<Vec<u8>, String> {
    let ticket = ticket
        .parse::<BlobTicket>()
        .map_err(|error| error.to_string())?;
    fetch_blob_ticket_bytes(blob_store, endpoint, &ticket).await
}

async fn fetch_blob_ticket_bytes(
    blob_store: &MemStore,
    endpoint: &Endpoint,
    ticket: &BlobTicket,
) -> Result<Vec<u8>, String> {
    if ticket.recursive() {
        return Err("blob format is not supported".to_string());
    }
    let connection = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .map_err(|error| error.to_string())?;
    let (size, _) = iroh_blobs::get::request::get_verified_size(&connection, &ticket.hash())
        .await
        .map_err(|error| error.to_string())?;
    if size > REMOTE_BLOB_MAX_BYTES as u64 {
        return Err("blob size is not supported".to_string());
    }
    let _read_guard = blob_store
        .tags()
        .temp_tag(ticket.hash_and_format())
        .await
        .map_err(|error| error.to_string())?;
    let downloader = blob_store.downloader(endpoint);
    downloader
        .download(ticket.hash(), Some(ticket.addr().id))
        .await
        .map_err(|error| error.to_string())?;
    let bytes = blob_store
        .export_bao(ticket.hash(), iroh_blobs::protocol::ChunkRanges::all())
        .data_to_bytes()
        .await
        .map_err(|error| error.to_string())?;
    if bytes.len() > REMOTE_BLOB_MAX_BYTES {
        return Err("blob size is not supported".to_string());
    }
    Ok(bytes.to_vec())
}

fn new_blob_store() -> MemStore {
    new_blob_store_with_gc_interval(BLOB_GC_INTERVAL)
}

fn new_blob_store_with_gc_interval(interval: Duration) -> MemStore {
    MemStore::new_with_opts(MemStoreOptions {
        gc_config: Some(GcConfig {
            interval,
            add_protected: None,
        }),
    })
}

fn spawn_path_watcher(connection: Connection, on_state: RemoteTransportStateHandler) {
    tokio::spawn(async move {
        let mut events = connection.path_events();
        while let Some(event) = events.next().await {
            match event {
                PathEvent::Selected { remote_addr, .. } => {
                    publish_transport_state_for_addr(&on_state, &remote_addr, None);
                }
                PathEvent::Lagged { .. } => publish_path_state(&connection, &on_state),
                _ => {}
            }
        }
    });
}

fn publish_path_state(connection: &Connection, on_state: &RemoteTransportStateHandler) {
    let paths = connection.paths();
    for path in paths.iter() {
        if path.is_selected() {
            publish_transport_state_for_addr(on_state, path.remote_addr(), Some(path.rtt()));
            return;
        }
    }
    on_state(String::new(), "connected:path=unknown".to_string());
}

fn publish_transport_state_for_addr(
    on_state: &RemoteTransportStateHandler,
    remote_addr: &TransportAddr,
    rtt: Option<std::time::Duration>,
) {
    let path = transport_path_kind(remote_addr);
    let addr = transport_addr_label(remote_addr);
    let relay_url = match remote_addr {
        TransportAddr::Relay(url) => Some(url.to_string()),
        _ => None,
    };
    let relay_url_detail = relay_url
        .as_deref()
        .map(|url| format!(";relayUrl={url}"))
        .unwrap_or_default();
    if let Some(rtt) = rtt {
        on_state(
            String::new(),
            format!(
                "latency:rtt={};path={path};addr={}{}",
                rtt.as_millis(),
                addr,
                relay_url_detail
            ),
        );
    }
    on_state(
        String::new(),
        format!("connected:path={path};addr={}{}", addr, relay_url_detail),
    );
}

/// How a peer is reached right now. The host logs it and the controller reports
/// it to the UI, so both read the same rule.
fn transport_path_kind(remote_addr: &TransportAddr) -> &'static str {
    if remote_addr.is_relay() {
        "relay"
    } else if remote_addr.is_ip() {
        "direct"
    } else {
        "unknown"
    }
}

fn transport_addr_label(remote_addr: &TransportAddr) -> String {
    match remote_addr {
        TransportAddr::Ip(addr) => socket_addr_label(addr),
        TransportAddr::Relay(url) => url.to_string(),
        TransportAddr::Custom(addr) => addr.to_string(),
        _ => remote_addr.to_string(),
    }
}

fn socket_addr_label(addr: &SocketAddr) -> String {
    match addr {
        SocketAddr::V4(addr) => addr.to_string(),
        SocketAddr::V6(addr) => format!("[{}]:{}", addr.ip(), addr.port()),
    }
}

/// Best-effort: add LAN mDNS address lookup so two endpoints on the same local
/// network discover each other's direct addresses and connect WITHOUT routing
/// through the relay (lower latency, and it works even when the relay is the
/// slow path). This is purely additive to the N0 preset's relay + DNS discovery
/// — if mDNS can't initialize (no usable IPv4/IPv6, multicast blocked) the
/// endpoint keeps working over the relay, so errors are logged and swallowed.
///
/// Must be called from within a tokio runtime (after `bind().await`): the mDNS
/// service spawns onto the current runtime handle.
fn enable_local_discovery(endpoint: &Endpoint, on_log: &Option<RemoteTransportLogHandler>) {
    if std::env::var(IROH_DISABLE_LAN_MDNS_ENV)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
    {
        log_transport(on_log, "iroh_lan_mdns disabled via env".to_string());
        return;
    }
    let lookup = match endpoint.address_lookup() {
        Ok(lookup) => lookup,
        Err(error) => {
            log_transport(on_log, format!("iroh_lan_mdns unavailable error={error}"));
            return;
        }
    };
    match MdnsAddressLookup::builder().build(endpoint.id()) {
        Ok(mdns) => {
            lookup.add(mdns);
            log_transport(on_log, "iroh_lan_mdns enabled".to_string());
        }
        Err(error) => {
            log_transport(on_log, format!("iroh_lan_mdns init failed error={error}"));
        }
    }
}

fn endpoint_builder(
    relay_url: Option<&RelayUrl>,
    relay_authentication: &str,
    secret_key: Option<&SecretKey>,
) -> iroh::endpoint::Builder {
    let mut builder = Endpoint::builder(presets::N0);
    if let Some(secret_key) = secret_key {
        builder = builder.secret_key(secret_key.clone());
    }
    if let Some(relay_url) = relay_url {
        let mut relay_map = RelayMap::from(relay_url.clone());
        let relay_authentication = relay_authentication.trim();
        if !relay_authentication.is_empty() {
            relay_map = relay_map.with_auth_token(relay_authentication.to_string());
        }
        builder = builder.relay_mode(RelayMode::Custom(relay_map));
    }
    builder
}

pub(crate) fn host_secret_key(config: &RemoteHostTransportConfig) -> Option<SecretKey> {
    let host_token = config.host_token.trim();
    if host_token.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(b"codux-iroh-host-secret-key-v1");
    hasher.update(config.host_id.trim().as_bytes());
    hasher.update([0]);
    hasher.update(host_token.as_bytes());
    let bytes: [u8; 32] = hasher.finalize().into();
    Some(SecretKey::from_bytes(&bytes))
}

fn iroh_relay_url(config: &RemoteHostTransportConfig) -> Result<Option<RelayUrl>, String> {
    let configured = std::env::var(IROH_RELAY_URL_ENV).unwrap_or_default();
    let configured = configured.trim();
    let configured = if configured.is_empty() {
        config.iroh_relay_url.trim()
    } else {
        configured
    };
    let resolved = if configured.is_empty() {
        crate::iroh_relay_url_for_preset(&config.relay_preset, "")
    } else {
        configured.to_string()
    };
    let trimmed = resolved.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    parse_relay_url(trimmed).map(Some)
}

/// Fully resolved iroh dial target.
///
/// Construction is the sole definition of dialability. Callers must not first
/// judge field presence: tickets, node ids, relays and remembered addresses are
/// only routes after they parse and agree on one endpoint identity. All usable
/// ticket, reported-relay and remembered-address routes are retained so iroh can
/// race them and keep whichever answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IrohDialTarget {
    endpoint_addr: EndpointAddr,
    /// Relay to use as this controller endpoint's home, preferring the host's
    /// current independently reported route over a stale ticket snapshot.
    controller_relay_url: Option<RelayUrl>,
}

pub fn build_iroh_dial_target(
    candidate: &RemoteTransportCandidate,
    remembered: &[String],
) -> Result<IrohDialTarget, String> {
    if candidate.kind != codux_protocol::REMOTE_TRANSPORT_IROH {
        return Err(format!(
            "unsupported remote transport kind: {}",
            candidate.kind
        ));
    }

    let ticket = candidate.ticket.trim();
    // A corrupt ticket may coexist with a usable refreshed node/relay pair, so
    // it degrades to those routes. A valid ticket, however, establishes an
    // identity that every redundant identity field must agree with.
    let from_ticket = if ticket.is_empty() {
        None
    } else {
        EndpointTicket::from_str(ticket)
            .map(EndpointAddr::from)
            .ok()
    };

    let node_id = candidate.node_id.trim();
    let parsed_node_id = if node_id.is_empty() {
        None
    } else {
        Some(
            node_id
                .parse()
                .map_err(|error| format!("invalid iroh node id: {error}"))?,
        )
    };

    let mut addr = match (from_ticket, parsed_node_id) {
        (Some(addr), Some(node_id)) if addr.id != node_id => {
            return Err(format!(
                "iroh candidate identity mismatch: ticket={} nodeId={node_id}",
                addr.id
            ));
        }
        (Some(addr), _) => addr,
        (None, Some(node_id)) => EndpointAddr::new(node_id),
        (None, None) if !ticket.is_empty() => {
            return Err("invalid iroh endpoint ticket and missing node id".to_string());
        }
        (None, None) => return Err("missing iroh endpoint identity".to_string()),
    };

    let reported_relay = match candidate.relay_url.trim() {
        "" => None,
        relay_url => parse_relay_url(relay_url).ok(),
    };
    if let Some(relay_url) = reported_relay.as_ref() {
        addr = addr.with_relay_url(relay_url.clone());
    }
    addr = with_remembered_addrs(addr, remembered);
    if addr.is_empty() {
        return Err(
            "iroh candidate has no route: no usable ticket, relay, or remembered address"
                .to_string(),
        );
    }
    let controller_relay_url = reported_relay.or_else(|| addr.relay_urls().next().cloned());
    Ok(IrohDialTarget {
        endpoint_addr: addr,
        controller_relay_url,
    })
}

fn parse_relay_url(value: &str) -> Result<RelayUrl, String> {
    value
        .parse::<RelayUrl>()
        .map_err(|error| format!("invalid iroh relay url `{value}`: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_less_dial_is_not_slower_than_the_relay_gate() {
        let schedule = DialSchedule::for_relay_state(false);
        assert!(
            schedule.total_budget() <= IROH_ONLINE_TIMEOUT,
            "a relay-less dial must not make the user wait longer for failure \
             than the relay gate it replaces: {:?} > {:?}",
            schedule.total_budget(),
            IROH_ONLINE_TIMEOUT
        );
    }

    #[test]
    fn relay_less_dial_is_flat_and_short() {
        let schedule = DialSchedule::for_relay_state(false);
        assert!(
            schedule.attempts >= 2,
            "one dropped packet must not decide it"
        );
        assert_eq!(
            schedule.timeout_for(0),
            schedule.timeout_for(schedule.attempts - 1),
            "direct addresses answer within an RTT or not at all; escalating \
             only delays the failure"
        );
    }

    #[test]
    fn relay_routed_dial_keeps_its_escalating_ladder() {
        let schedule = DialSchedule::for_relay_state(true);
        assert_eq!(schedule.attempts, 3);
        assert_eq!(schedule.timeout_for(0), Duration::from_secs(4));
        assert_eq!(schedule.timeout_for(1), Duration::from_secs(6));
        assert_eq!(schedule.timeout_for(2), Duration::from_secs(8));
        assert_eq!(schedule.total_budget(), Duration::from_secs(18));
    }

    #[test]
    fn dial_exhausted_error_names_the_path_that_was_tried() {
        let relayed = dial_exhausted_error(true, None);
        let direct = dial_exhausted_error(false, None);
        assert_ne!(
            relayed, direct,
            "a relay-less failure must not read as a plain relay timeout"
        );
        assert!(
            direct.contains("no relay home"),
            "the relay-less error must say why only direct addresses could have \
             worked: {direct}"
        );
        assert_eq!(
            relayed, "iroh controller connect timeout",
            "the relayed error string is load-bearing for existing log analysis"
        );
    }

    #[test]
    fn an_immediate_relay_less_failure_keeps_the_route_class() {
        let error = dial_exhausted_error(false, Some("connection refused"));

        assert!(error.contains("no relay home"), "{error}");
        assert!(error.contains("connection refused"), "{error}");
    }

    #[test]
    fn dial_timeout_saturates_past_the_last_attempt() {
        let schedule = DialSchedule::for_relay_state(true);
        assert_eq!(
            schedule.timeout_for(99),
            schedule.timeout_for(schedule.attempts - 1),
            "an out-of-range attempt index must not panic or grow unbounded"
        );
    }

    fn iroh_candidate(node_id: &str, relay_url: &str, ticket: &str) -> RemoteTransportCandidate {
        RemoteTransportCandidate {
            kind: codux_protocol::REMOTE_TRANSPORT_IROH.to_string(),
            url: String::new(),
            node_id: node_id.to_string(),
            relay_url: relay_url.to_string(),
            ticket: ticket.to_string(),
            relay_authentication: String::new(),
        }
    }

    fn test_endpoint_id() -> iroh::EndpointId {
        SecretKey::from_bytes(&[7u8; 32]).public()
    }

    fn other_test_endpoint_id() -> iroh::EndpointId {
        SecretKey::from_bytes(&[8u8; 32]).public()
    }

    fn endpoint_ticket(addr: EndpointAddr) -> String {
        EndpointTicket::from(addr).to_string()
    }

    #[test]
    fn controller_selection_continues_after_an_invalid_candidate() {
        let ticket = endpoint_ticket(
            EndpointAddr::new(test_endpoint_id())
                .with_ip_addr("192.168.1.5:41234".parse().unwrap()),
        );
        let invalid = iroh_candidate(
            &other_test_endpoint_id().to_string(),
            "https://relay.example",
            &ticket,
        );
        let valid = iroh_candidate(&test_endpoint_id().to_string(), "https://relay.example", "");
        let config = RemoteControllerTransportConfig {
            transports: vec![invalid, valid],
            ..Default::default()
        };

        let (selected, target) =
            select_controller_dial_target(&config).expect("second candidate is dialable");

        assert_eq!(selected, &config.transports[1]);
        assert_eq!(target.endpoint_addr.id, test_endpoint_id());
    }

    #[test]
    fn controller_selection_preserves_the_first_constructor_error() {
        let ticket = endpoint_ticket(
            EndpointAddr::new(test_endpoint_id())
                .with_ip_addr("192.168.1.5:41234".parse().unwrap()),
        );
        let first = iroh_candidate(
            &other_test_endpoint_id().to_string(),
            "https://relay.example",
            &ticket,
        );
        let expected = build_iroh_dial_target(&first, &[]).unwrap_err();
        let config = RemoteControllerTransportConfig {
            transports: vec![first, iroh_candidate("invalid-node-id", "", "")],
            ..Default::default()
        };

        assert_eq!(
            select_controller_dial_target(&config).unwrap_err(),
            expected
        );
    }

    #[test]
    fn controller_selection_reports_a_missing_candidate_only_for_an_empty_list() {
        let config = RemoteControllerTransportConfig::default();

        assert_eq!(
            select_controller_dial_target(&config).unwrap_err(),
            "missing usable iroh transport candidate"
        );
    }

    #[test]
    fn remembered_addresses_join_the_dial_set() {
        // The ticket is a pairing-time snapshot. An address that carried a
        // working connection since is the cheapest evidence available that the
        // host may still be there.
        let ticket_addr = EndpointAddr::new(test_endpoint_id())
            .with_ip_addr("192.168.1.5:41234".parse().unwrap());

        let merged = with_remembered_addrs(ticket_addr, &["10.8.0.3:41234".to_string()]);

        let addrs: Vec<String> = merged.ip_addrs().map(|addr| addr.to_string()).collect();
        assert!(addrs.contains(&"192.168.1.5:41234".to_string()));
        assert!(addrs.contains(&"10.8.0.3:41234".to_string()));
    }

    #[test]
    fn a_remembered_address_the_ticket_already_has_is_not_dialled_twice() {
        let ticket_addr = EndpointAddr::new(test_endpoint_id())
            .with_ip_addr("192.168.1.5:41234".parse().unwrap());

        let merged = with_remembered_addrs(ticket_addr, &["192.168.1.5:41234".to_string()]);

        assert_eq!(merged.ip_addrs().count(), 1);
    }

    #[test]
    fn an_unusable_remembered_address_does_not_cost_the_connection() {
        // Stored data outlives the code that wrote it. One unparseable row must
        // not fail a dial that the ticket alone could have completed.
        let ticket_addr = EndpointAddr::new(test_endpoint_id())
            .with_ip_addr("192.168.1.5:41234".parse().unwrap());

        let merged = with_remembered_addrs(
            ticket_addr,
            &[
                "not-an-address".to_string(),
                String::new(),
                "   ".to_string(),
                "10.8.0.3:41234".to_string(),
            ],
        );

        let addrs: Vec<String> = merged.ip_addrs().map(|addr| addr.to_string()).collect();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.contains(&"10.8.0.3:41234".to_string()));
    }

    #[test]
    fn relay_urls_survive_the_merge() {
        let ticket_addr = EndpointAddr::new(test_endpoint_id())
            .with_relay_url(parse_relay_url("https://relay.example").expect("relay url"));

        let merged = with_remembered_addrs(ticket_addr, &["10.8.0.3:41234".to_string()]);

        assert_eq!(merged.relay_urls().count(), 1);
        assert_eq!(merged.ip_addrs().count(), 1);
    }

    #[test]
    fn remembered_addresses_make_a_relay_less_candidate_dialable() {
        let candidate = iroh_candidate(&test_endpoint_id().to_string(), "", "");

        assert!(build_iroh_dial_target(&candidate, &["10.8.0.3:41234".to_string()]).is_ok());
        assert!(build_iroh_dial_target(&candidate, &[]).is_err());
    }

    #[test]
    fn a_node_id_and_a_remembered_address_build_a_dial_target() {
        let candidate = iroh_candidate(&test_endpoint_id().to_string(), "", "");

        let target = build_iroh_dial_target(&candidate, &["10.8.0.3:41234".to_string()])
            .expect("node id + remembered address is dialable");
        let addr = target.endpoint_addr;

        assert_eq!(addr.ip_addrs().count(), 1);
        assert_eq!(addr.relay_urls().count(), 0);
    }

    #[test]
    fn a_corrupt_ticket_falls_back_instead_of_failing_the_dial() {
        // A ticket that will not parse says nothing about the node id and relay
        // stored beside it. Giving up there threw away a working route.
        let candidate = iroh_candidate(
            &test_endpoint_id().to_string(),
            "https://relay.example",
            "not-a-ticket",
        );

        let target =
            build_iroh_dial_target(&candidate, &[]).expect("must fall back to node id + relay");
        let addr = target.endpoint_addr;

        assert_eq!(addr.relay_urls().count(), 1);
    }

    #[test]
    fn a_candidate_with_no_route_at_all_is_refused() {
        let candidate = iroh_candidate(&test_endpoint_id().to_string(), "", "");

        assert!(build_iroh_dial_target(&candidate, &[]).is_err());
    }

    #[test]
    fn a_valid_ticket_unions_the_independent_relay_route() {
        let ticket = endpoint_ticket(
            EndpointAddr::new(test_endpoint_id())
                .with_ip_addr("192.168.1.5:41234".parse().unwrap()),
        );
        let candidate = iroh_candidate(
            &test_endpoint_id().to_string(),
            "https://relay.example",
            &ticket,
        );

        let target =
            build_iroh_dial_target(&candidate, &[]).expect("ticket plus relay is dialable");
        let addr = target.endpoint_addr;

        assert_eq!(addr.id, test_endpoint_id());
        assert_eq!(addr.ip_addrs().count(), 1);
        assert_eq!(addr.relay_urls().count(), 1);
    }

    #[test]
    fn a_ticket_and_node_id_must_name_the_same_endpoint() {
        let ticket = endpoint_ticket(
            EndpointAddr::new(test_endpoint_id())
                .with_ip_addr("192.168.1.5:41234".parse().unwrap()),
        );
        let candidate = iroh_candidate(
            &other_test_endpoint_id().to_string(),
            "https://relay.example",
            &ticket,
        );

        let error = build_iroh_dial_target(&candidate, &[])
            .expect_err("conflicting endpoint identities must fail closed");

        assert!(error.contains("identity mismatch"), "{error}");
    }

    #[test]
    fn malformed_routes_do_not_make_a_candidate_dialable() {
        let candidate = iroh_candidate("not-a-node-id", "not-a-relay-url", "not-a-ticket");

        assert!(build_iroh_dial_target(&candidate, &["not-a-socket-address".to_string()]).is_err());
    }

    #[test]
    fn a_ticket_alone_is_enough_to_dial() {
        // The host may have no relay at all — a LAN-only or SD-WAN-only
        // deployment. Its ticket still serialises the direct addresses, which is
        // everything `connect` needs.
        let ticket = endpoint_ticket(
            EndpointAddr::new(test_endpoint_id())
                .with_ip_addr("192.168.1.5:41234".parse().unwrap()),
        );
        assert!(build_iroh_dial_target(&iroh_candidate("", "", &ticket), &[]).is_ok());
    }

    #[test]
    fn node_id_needs_a_relay_when_there_is_no_ticket() {
        // Without a ticket there are no direct addresses, so the only way to
        // reach the peer is to route through its relay.
        assert!(
            build_iroh_dial_target(
                &iroh_candidate(&test_endpoint_id().to_string(), "https://relay.example", ""),
                &[]
            )
            .is_ok()
        );
        assert!(
            build_iroh_dial_target(
                &iroh_candidate(&test_endpoint_id().to_string(), "", ""),
                &[]
            )
            .is_err()
        );
        assert!(
            build_iroh_dial_target(&iroh_candidate("", "https://relay.example", ""), &[]).is_err()
        );
    }

    #[test]
    fn a_candidate_of_another_kind_is_never_dialable_here() {
        let mut candidate =
            iroh_candidate(&test_endpoint_id().to_string(), "https://relay.example", "");
        candidate.kind = "something-else".to_string();
        assert!(build_iroh_dial_target(&candidate, &[]).is_err());
    }

    #[test]
    fn whitespace_is_not_a_ticket() {
        assert!(build_iroh_dial_target(&iroh_candidate("", "", "   "), &[]).is_err());
    }

    #[tokio::test]
    async fn retiring_the_pool_entry_leaves_a_live_connection_serving_blobs() {
        // The pool retires an entry when the relay changes or a connect
        // exhausts its dials — neither says anything about connections already
        // running on that endpoint. Owning the accept loop from the entry meant
        // retiring it stopped blob serving underneath them, so uploads and file
        // writes failed on a link that was otherwise healthy.
        let endpoint = direct_endpoint(vec![iroh_blobs::ALPN.to_vec()]).await;
        let blob_store = new_blob_store();
        let router = Arc::new(
            Router::builder(endpoint.clone())
                .accept(
                    iroh_blobs::ALPN,
                    BlobsProtocol::new(blob_store.as_ref(), None),
                )
                .spawn(),
        );

        // What a live transport holds.
        let held_by_connection = Arc::clone(&router);
        // What the pool holds, and then drops when the entry is retired.
        let held_by_pool = Arc::clone(&router);
        drop(router);
        drop(held_by_pool);

        assert_eq!(
            Arc::strong_count(&held_by_connection),
            1,
            "the connection's reference must be the one keeping the loop alive"
        );

        // Still serving: a blob published now is fetchable over this endpoint.
        let ticket = publish_blob_bytes(&blob_store, &endpoint, b"after-evict".to_vec())
            .await
            .expect("publish");
        let peer_store = new_blob_store();
        let peer = direct_endpoint(Vec::new()).await;
        let bytes = fetch_blob_bytes(&peer_store, &peer, &ticket)
            .await
            .expect("the accept loop must outlive the pool entry");
        assert_eq!(bytes, b"after-evict".to_vec());

        drop(held_by_connection);
        peer.close().await;
        endpoint.close().await;
    }

    #[tokio::test]
    async fn terminal_upload_blob_round_trips_top_level_request_id() {
        let endpoint = direct_endpoint(vec![iroh_blobs::ALPN.to_vec()]).await;
        let blob_store = new_blob_store();
        let (control_tx, mut control_rx) = mpsc::channel(1);
        let credentials = Arc::new(Mutex::new(ControllerCredentials {
            device_id: "device-1".to_string(),
            device_token: "token-1".to_string(),
        }));

        send_terminal_upload_blob(
            &blob_store,
            &endpoint,
            &control_tx,
            &credentials,
            RemoteTransportUpload {
                device_id: String::new(),
                session_id: "session-1".to_string(),
                request_id: Some("upload-request-1".to_string()),
                name: "photo.png".to_string(),
                mime: "image/png".to_string(),
                kind: "image".to_string(),
                bytes: vec![1, 2, 3],
                ticket: String::new(),
            },
        )
        .await
        .expect("send upload envelope");

        let bytes = control_rx.recv().await.expect("upload envelope");
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("upload JSON");
        assert_eq!(value["requestId"], "upload-request-1");
        assert!(value["payload"].get("requestId").is_none());

        let envelope: RemoteEnvelope = serde_json::from_value(value).expect("remote envelope");
        let upload =
            upload_metadata_from_blob_envelope(&envelope, "fallback-device").expect("metadata");
        assert_eq!(upload.request_id.as_deref(), Some("upload-request-1"));
        assert_eq!(upload.device_id, "device-1");
        assert_eq!(upload.session_id, "session-1");
        assert_eq!(upload.name, "photo.png");
        assert_eq!(upload.mime, "image/png");
        assert_eq!(upload.kind, "image");
        assert!(!upload.ticket.is_empty());
        endpoint.close().await;
    }

    #[tokio::test]
    async fn queued_upload_worker_failure_is_published_to_controller_once() {
        let endpoint = direct_endpoint(Vec::new()).await;
        let blob_store = new_blob_store();
        let (control_tx, _control_rx) = mpsc::channel(1);
        control_tx
            .try_send(b"occupied".to_vec())
            .expect("fill control queue");
        let (upload_tx, upload_rx) = mpsc::channel(1);
        let credentials = Arc::new(Mutex::new(ControllerCredentials {
            device_id: "credential-device".to_string(),
            device_token: "token-1".to_string(),
        }));
        let (message_tx, mut message_rx) = mpsc::unbounded_channel();
        let on_message: RemoteTransportMessageHandler = Arc::new(move |device_id, data| {
            message_tx.send((device_id, data)).unwrap();
        });
        spawn_upload_writer(
            blob_store,
            endpoint.clone(),
            upload_rx,
            control_tx,
            credentials,
            on_message,
            None,
        );

        let enqueued = upload_tx
            .try_send(RemoteTransportUpload {
                device_id: "original-device".to_string(),
                session_id: "session-1".to_string(),
                request_id: Some("upload-request-1".to_string()),
                name: "photo.png".to_string(),
                mime: "image/png".to_string(),
                kind: "image".to_string(),
                bytes: vec![1, 2, 3],
                ticket: String::new(),
            })
            .is_ok();
        assert!(
            enqueued,
            "upload enqueue result remains transport acceptance"
        );

        let (callback_device_id, data) =
            tokio::time::timeout(Duration::from_secs(2), message_rx.recv())
                .await
                .expect("upload failure callback timeout")
                .expect("upload failure callback");
        let error: serde_json::Value = serde_json::from_slice(&data).expect("error envelope");
        assert_eq!(callback_device_id, "original-device");
        assert_eq!(error["type"], codux_protocol::REMOTE_ERROR);
        assert_eq!(error["deviceId"], "original-device");
        assert_eq!(error["sessionId"], "session-1");
        assert_eq!(error["requestId"], "upload-request-1");
        assert_eq!(error["payload"]["code"], "terminal_upload_failed");
        assert!(
            error["payload"]["message"]
                .as_str()
                .is_some_and(|message| message.contains("no available capacity"))
        );
        tokio::task::yield_now().await;
        assert!(message_rx.try_recv().is_err(), "failure must publish once");
        drop(upload_tx);
        endpoint.close().await;
    }

    #[test]
    fn terminal_upload_metadata_accepts_legacy_missing_request_id() {
        let envelope = RemoteEnvelope {
            kind: REMOTE_TERMINAL_UPLOAD_BLOB.to_string(),
            device_id: Some("device-1".to_string()),
            session_id: Some("session-1".to_string()),
            request_id: None,
            seq: None,
            payload: serde_json::json!({
                "name": "photo.png",
                "ticket": "ticket",
            }),
        };

        let upload =
            upload_metadata_from_blob_envelope(&envelope, "fallback-device").expect("metadata");
        assert_eq!(upload.request_id, None);
    }

    #[tokio::test]
    async fn upload_result_follows_the_device_to_its_current_connection() {
        // A phone that loses its link mid-upload comes back on a fresh
        // connection. The host still finishes the download, writes the file and
        // injects the path into the PTY, so the reply has to reach whoever owns
        // the device alias now — otherwise the phone times out, retries, and the
        // host writes a second file and injects a second path.
        let host = test_host().await;
        let peer = test_peer(&host.transport).await;

        let (origin, mut origin_rx) = mpsc::channel::<Vec<u8>>(1);
        let (origin_terminal, _origin_terminal_rx) = mpsc::channel::<Vec<u8>>(1);
        let origin_senders = PeerSenders {
            id: HOST_PEER_ID.fetch_add(2, Ordering::Relaxed),
            connection: peer.connection.clone(),
            control: origin,
            terminal: origin_terminal,
            closed: Arc::new(AtomicBool::new(false)),
        };
        host.transport
            .peers
            .lock()
            .unwrap()
            .insert("device-1".to_string(), origin_senders.clone());

        // The link drops and the phone reconnects: the alias is rebound while
        // the upload is still being written.
        origin_senders.closed.store(true, Ordering::SeqCst);
        let (replacement, mut replacement_rx) = mpsc::channel::<Vec<u8>>(1);
        let (replacement_terminal, _replacement_terminal_rx) = mpsc::channel::<Vec<u8>>(1);
        host.transport.peers.lock().unwrap().insert(
            "device-1".to_string(),
            PeerSenders {
                id: HOST_PEER_ID.fetch_add(2, Ordering::Relaxed),
                connection: peer.connection.clone(),
                control: replacement,
                terminal: replacement_terminal,
                closed: Arc::new(AtomicBool::new(false)),
            },
        );

        publish_terminal_upload_reply(
            Arc::clone(&host.transport),
            TerminalUploadReply {
                device_id: "device-1".to_string(),
                session_id: "session-1".to_string(),
                request_id: Some("upload-1".to_string()),
            },
            Ok(RemoteTransportUploadOutcome {
                path: "/tmp/photo.png".to_string(),
                name: "photo.png".to_string(),
                kind: "image".to_string(),
            }),
            None,
        )
        .await;

        let response = tokio::time::timeout(Duration::from_secs(2), replacement_rx.recv())
            .await
            .expect(
                "the reply never reached the connection that owns the device now, so the \
                 reconnected phone waits out its timeout and retries an upload the host \
                 has already written and injected",
            )
            .expect("upload reply");
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["type"], codux_protocol::REMOTE_TERMINAL_UPLOADED);
        assert_eq!(response["requestId"], "upload-1");
        assert_eq!(response["payload"]["inserted"], true);
        assert!(
            origin_rx.try_recv().is_err(),
            "the retired connection must not receive the reply"
        );
    }

    #[test]
    fn legacy_upload_reply_omits_request_id() {
        let response = terminal_upload_reply_data(
            &TerminalUploadReply {
                device_id: "device-1".to_string(),
                session_id: "session-1".to_string(),
                request_id: None,
            },
            &Err("PTY write failed".to_string()),
        )
        .expect("reply");
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();

        assert_eq!(response["type"], codux_protocol::REMOTE_ERROR);
        assert_eq!(response["payload"]["code"], "terminal_upload_failed");
        assert!(response.get("requestId").is_none());
    }

    #[tokio::test]
    async fn relay_state_is_read_live_from_the_endpoint() {
        // A relay-disabled endpoint can never have a home, so this pins the
        // reader to the endpoint's own state rather than a build-time flag —
        // the property that lets a recovered relay change the schedule back.
        let endpoint = direct_endpoint(Vec::new()).await;
        assert!(connected_relay_url(&endpoint).is_none());
        assert_eq!(
            DialSchedule::for_relay_state(connected_relay_url(&endpoint).is_some()),
            DialSchedule::for_relay_state(false)
        );
        endpoint.close().await;
    }

    #[test]
    fn only_connected_home_relays_are_live_routes() {
        let connecting = parse_relay_url("https://connecting.example").expect("relay url");
        let connected = parse_relay_url("https://connected.example").expect("relay url");

        assert_eq!(
            select_connected_relay_url([(connecting.clone(), false), (connected.clone(), true)]),
            Some(connected)
        );
        assert_eq!(select_connected_relay_url([(connecting, false)]), None);
    }

    #[tokio::test]
    async fn a_relay_less_host_publishes_a_ticket_only_candidate() {
        // The host reports these to controllers, which replace their stored
        // dial set from them. With no relay the candidate must still be one a
        // controller will accept, or the report strands the device.
        let host = test_host().await;
        let candidate = host.transport.current_dial_candidate().expect("candidate");

        assert!(
            candidate.relay_url.is_empty(),
            "this host has no relay home"
        );
        assert!(!candidate.ticket.is_empty());
        assert!(build_iroh_dial_target(&candidate, &[]).is_ok());
    }

    #[tokio::test]
    async fn a_relay_that_never_connects_is_absent_from_everything_the_host_publishes() {
        // Two things hang on this. The QR serializer treats a non-empty
        // `relayUrl` as a usable route and drops the direct-address ticket, so
        // publishing a relay we have no path to produces an unusable QR. And
        // the phone replaces its stored candidate with whatever the host
        // reports, so if the ticket did not keep the configured relay, a host
        // that starts while its relay is down would erase the phone's only
        // relay route -- leaving it stranded the next time it has no direct
        // path.
        crate::install_rustls_crypto_provider();
        use iroh::RelayConfig;
        let relay: RelayUrl = "https://relay.invalid.example"
            .parse()
            .expect("relay url");
        let endpoint = Endpoint::builder(presets::N0)
            .alpns(vec![CODUX_REMOTE_ALPN.to_vec()])
            .relay_mode(RelayMode::Custom(RelayMap::from_iter([RelayConfig::new(
                relay.clone(),
                None,
            )])))
            .bind()
            .await
            .expect("bind endpoint with an unroutable relay");

        assert!(
            connected_relay_url(&endpoint).is_none(),
            "a relay that never completes must not read as connected"
        );
        assert_eq!(
            endpoint.addr().relay_urls().count(),
            0,
            "a configured relay that never completed is not in the endpoint's own \
             address set either, so the ticket cannot carry it as a fallback -- \
             whatever the phone had stored is all it will have"
        );
    }

    #[tokio::test]
    async fn the_published_candidate_agrees_with_its_endpoint() {
        // Node and ticket describe one known-address snapshot; the top-level
        // relay independently describes a connected home. A configured-but-
        // offline relay must not appear there: the QR serializer would treat it
        // as a usable route and omit the direct-address ticket.
        let host = test_host().await;
        let candidate = host.transport.current_dial_candidate().expect("candidate");

        let live = host
            .transport
            .endpoint
            .home_relay_status()
            .get()
            .into_iter()
            .find(|status| status.is_connected())
            .map(|status| status.url().to_string())
            .unwrap_or_default();
        assert_eq!(candidate.relay_url, live);
        assert_eq!(candidate.node_id, host.transport.endpoint.id().to_string());
        assert_eq!(
            EndpointTicket::from_str(&candidate.ticket)
                .expect("candidate ticket")
                .endpoint_addr()
                .id,
            host.transport.endpoint.id()
        );
    }

    #[tokio::test]
    async fn relay_less_endpoints_still_connect_to_each_other() {
        // The whole point of removing the relay gate: no relay anywhere, and the
        // dial still completes from the addresses in the peer's `EndpointAddr`.
        let host = test_host().await;
        assert!(connected_relay_url(&host.transport.endpoint).is_none());
        let peer = test_peer(&host.transport).await;
        assert!(connected_relay_url(&peer.endpoint).is_none());
        assert_eq!(peer.connection.remote_id(), host.transport.endpoint.id());
    }

    struct TestHost {
        transport: Arc<RemoteIrohHostTransport>,
        states: mpsc::UnboundedReceiver<(String, String)>,
        messages: mpsc::UnboundedReceiver<(String, Vec<u8>)>,
        logs: mpsc::UnboundedReceiver<String>,
    }

    struct TestPeer {
        endpoint: Endpoint,
        connection: Connection,
        control_send: SendStream,
        control_recv: RecvStream,
        _terminal_send: SendStream,
        terminal_recv: RecvStream,
    }

    async fn direct_endpoint(alpns: Vec<Vec<u8>>) -> Endpoint {
        crate::install_rustls_crypto_provider();
        Endpoint::builder(presets::N0)
            .alpns(alpns)
            .relay_mode(RelayMode::Disabled)
            .bind()
            .await
            .expect("bind direct iroh endpoint")
    }

    async fn test_host() -> TestHost {
        test_host_with_pairing(Arc::new(|_| None)).await
    }

    async fn test_host_with_pairing(on_pairing: RemoteTransportPairingHandler) -> TestHost {
        let endpoint = direct_endpoint(vec![CODUX_REMOTE_ALPN.to_vec()]).await;
        let (state_tx, states) = mpsc::unbounded_channel();
        let (message_tx, messages) = mpsc::unbounded_channel();
        let (log_tx, logs) = mpsc::unbounded_channel();
        let transport = Arc::new(RemoteIrohHostTransport {
            endpoint: endpoint.clone(),
            blob_store: new_blob_store(),
            peer_ownership: Mutex::new(()),
            peers: Mutex::new(HashMap::new()),
            on_message: Arc::new(move |device_id, data| {
                let _ = message_tx.send((device_id, data));
            }),
            on_upload: Arc::new(|_| Err("upload unsupported in test host".to_string())),
            on_state: Arc::new(move |device_id, state| {
                let _ = state_tx.send((device_id, state));
            }),
            on_pairing,
            on_authorize: Arc::new(|_, _| true),
            on_web_tunnel_tcp_connect: None,
            on_log: Some(Arc::new(move |message| {
                let _ = log_tx.send(message);
            })),
            router: OnceLock::new(),
        });
        let weak = Arc::downgrade(&transport);
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let Ok(connection) = incoming.await else {
                    continue;
                };
                let Some(transport) = weak.upgrade() else {
                    break;
                };
                tokio::spawn(transport.handle_connection(connection));
            }
        });
        TestHost {
            transport,
            states,
            messages,
            logs,
        }
    }

    async fn test_peer(host: &RemoteIrohHostTransport) -> TestPeer {
        let endpoint = direct_endpoint(Vec::new()).await;
        let connection = endpoint
            .connect(host.endpoint.addr(), CODUX_REMOTE_ALPN)
            .await
            .expect("connect direct iroh peer");
        let ((control_send, control_recv), (terminal_send, terminal_recv)) = tokio::time::timeout(
            Duration::from_secs(1),
            initialize_controller_streams(&connection),
        )
        .await
        .expect("stream handshake timeout")
        .expect("stream handshake");
        TestPeer {
            endpoint,
            connection,
            control_send,
            control_recv,
            _terminal_send: terminal_send,
            terminal_recv,
        }
    }

    async fn assert_missing_stream_times_out(kind: u8, label: &'static str) {
        let mut host = test_host().await;
        let endpoint = direct_endpoint(Vec::new()).await;
        let connection = endpoint
            .connect(host.transport.endpoint.addr(), CODUX_REMOTE_ALPN)
            .await
            .expect("connect direct iroh peer");
        let (send, recv) = connection.open_bi().await.expect("open stream");
        let _stream = tokio::time::timeout(
            Duration::from_secs(1),
            initialize_controller_stream(send, recv, kind, label),
        )
        .await
        .expect("stream ACK timeout")
        .expect("stream ACK");

        assert!(!host.transport.send_terminal(vec![1], None));
        let timeout_log =
            tokio::time::timeout(IROH_STREAM_READY_TIMEOUT + Duration::from_secs(1), async {
                loop {
                    let log = host.logs.recv().await.expect("host log channel");
                    if log.contains("iroh_host_stream_ready timeout") {
                        return log;
                    }
                }
            })
            .await
            .expect("host stream readiness timeout log");
        assert!(timeout_log.contains("timeout"));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), host.states.recv())
                .await
                .is_err()
        );

        connection.close(0u32.into(), b"test complete");
        endpoint.close().await;
        host.transport.shutdown().await;
    }

    async fn send_device_message(peer: &mut TestPeer, sequence: u64) -> Result<(), String> {
        let data = serde_json::to_vec(&serde_json::json!({
            "type": "test.message",
            "deviceId": "device-1",
            "deviceToken": "token-1",
            "payload": { "sequence": sequence },
        }))
        .unwrap();
        write_frame(&mut peer.control_send, &data).await
    }

    #[test]
    fn peer_streams_are_not_ready_until_control_and_terminal_are_staged() {
        let mut streams = PendingPeerStreams::new();

        streams.stage(STREAM_KIND_CONTROL, "control").unwrap();
        assert!(streams.take_ready().is_none());

        streams.stage(STREAM_KIND_TERMINAL, "terminal").unwrap();
        assert_eq!(streams.take_ready(), Some(("control", "terminal")));
        assert!(streams.stage(STREAM_KIND_TERMINAL, "duplicate").is_err());
    }

    #[test]
    fn peer_streams_can_arrive_terminal_first() {
        let mut streams = PendingPeerStreams::new();

        streams.stage(STREAM_KIND_TERMINAL, "terminal").unwrap();
        assert!(streams.take_ready().is_none());
        streams.stage(STREAM_KIND_CONTROL, "control").unwrap();

        assert_eq!(streams.take_ready(), Some(("control", "terminal")));
    }

    #[tokio::test]
    async fn host_times_out_when_terminal_stream_is_missing() {
        assert_missing_stream_times_out(STREAM_KIND_CONTROL, "control").await;
    }

    #[tokio::test]
    async fn host_times_out_when_control_stream_is_missing() {
        assert_missing_stream_times_out(STREAM_KIND_TERMINAL, "terminal").await;
    }

    #[tokio::test]
    async fn host_times_out_when_stream_kind_stalls() {
        let mut host = test_host().await;
        let endpoint = direct_endpoint(Vec::new()).await;
        let connection = endpoint
            .connect(host.transport.endpoint.addr(), CODUX_REMOTE_ALPN)
            .await
            .expect("connect direct iroh peer");
        let _stalled_stream = connection.open_bi().await.expect("open stalled stream");
        let (mut trigger_send, _trigger_recv) =
            connection.open_bi().await.expect("open trigger stream");
        write_stream_kind(&mut trigger_send, STREAM_KIND_CONTROL)
            .await
            .expect("make stalled stream visible");

        let timeout_log =
            tokio::time::timeout(IROH_STREAM_READY_TIMEOUT + Duration::from_secs(1), async {
                loop {
                    let log = host.logs.recv().await.expect("host log channel");
                    if log.contains("iroh_host_stream_kind timeout") {
                        return log;
                    }
                }
            })
            .await
            .expect("host stream kind timeout log");
        assert!(timeout_log.contains("timeout"));
        assert!(!host.transport.send_terminal(vec![1], None));

        connection.close(0u32.into(), b"test complete");
        endpoint.close().await;
        host.transport.shutdown().await;
    }

    #[tokio::test]
    async fn controller_streams_are_not_ready_before_both_acks() {
        let server = direct_endpoint(vec![CODUX_REMOTE_ALPN.to_vec()]).await;
        let client = direct_endpoint(Vec::new()).await;
        let (terminal_seen_tx, mut terminal_seen_rx) = mpsc::channel(1);
        let (terminal_ack_tx, mut terminal_ack_rx) = mpsc::channel(1);
        let server_endpoint = server.clone();
        let server_task = tokio::spawn(async move {
            let connection = server_endpoint
                .accept()
                .await
                .expect("incoming connection")
                .await
                .expect("accept connection");
            let (mut control_send, mut control_recv) =
                connection.accept_bi().await.expect("accept control stream");
            assert_eq!(
                read_stream_kind(&mut control_recv).await.unwrap(),
                STREAM_KIND_CONTROL
            );
            write_stream_kind(&mut control_send, STREAM_KIND_CONTROL)
                .await
                .unwrap();
            let (mut terminal_send, mut terminal_recv) = connection
                .accept_bi()
                .await
                .expect("accept terminal stream");
            assert_eq!(
                read_stream_kind(&mut terminal_recv).await.unwrap(),
                STREAM_KIND_TERMINAL
            );
            terminal_seen_tx.send(()).await.unwrap();
            terminal_ack_rx.recv().await.expect("terminal ACK release");
            write_stream_kind(&mut terminal_send, STREAM_KIND_TERMINAL)
                .await
                .unwrap();
            connection.closed().await;
        });
        let connection = client
            .connect(server.addr(), CODUX_REMOTE_ALPN)
            .await
            .expect("connect direct iroh peer");
        let ready_connection = connection.clone();
        let mut ready_task =
            tokio::spawn(async move { initialize_controller_streams(&ready_connection).await });

        terminal_seen_rx.recv().await.expect("terminal stream kind");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut ready_task)
                .await
                .is_err()
        );
        terminal_ack_tx.send(()).await.unwrap();
        let _streams = tokio::time::timeout(Duration::from_secs(1), ready_task)
            .await
            .expect("controller readiness timeout")
            .expect("controller readiness task")
            .expect("controller stream handshake");

        connection.close(0u32.into(), b"test complete");
        server_task.await.unwrap();
        client.close().await;
        server.close().await;
    }

    #[tokio::test]
    async fn host_reports_how_a_peer_is_reached() {
        // "Connected but slow" and "never upgraded off the relay" look the same
        // from the outside; the host knows which path QUIC picked, so it says.
        let mut host = test_host().await;
        let mut peer = test_peer(&host.transport).await;
        send_device_message(&mut peer, 1).await.unwrap();

        let path_log = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let log = host.logs.recv().await.expect("host log channel");
                if log.contains("iroh_host_path") {
                    return log;
                }
            }
        })
        .await
        .expect("host path log");

        assert!(path_log.contains("path=direct"), "unexpected: {path_log}");
        assert!(path_log.contains("rtt_ms="), "unexpected: {path_log}");

        peer.connection.close(0u32.into(), b"test complete");
        peer.endpoint.close().await;
        host.transport.shutdown().await;
    }

    #[tokio::test]
    async fn losing_one_stream_closes_the_whole_peer() {
        // The two streams share one peer record. Retiring the peer while its
        // connection stays open leaves the surviving reader dropping frames the
        // client believes were delivered, so the client never learns to
        // reconnect. Ending the connection is what tells it.
        let mut host = test_host().await;
        let mut peer = test_peer(&host.transport).await;
        send_device_message(&mut peer, 1).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), host.messages.recv())
            .await
            .expect("first host message timeout")
            .expect("first host message");

        // The terminal stream dies on its own; the control stream is untouched.
        peer._terminal_send.reset(0u32.into()).ok();

        tokio::time::timeout(Duration::from_secs(5), peer.connection.closed())
            .await
            .expect("host closes the connection after a stream ends");
        assert!(!host.transport.send(vec![2], None));

        peer.endpoint.close().await;
        host.transport.shutdown().await;
    }

    #[tokio::test]
    async fn newer_connection_keeps_device_alias_from_old_connection() {
        let mut host = test_host().await;
        let mut old = test_peer(&host.transport).await;
        send_device_message(&mut old, 1).await.unwrap();
        let (_, first) = tokio::time::timeout(Duration::from_secs(1), host.messages.recv())
            .await
            .expect("first host message timeout")
            .expect("first host message");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&first).unwrap()["payload"]["sequence"],
            1
        );

        let mut new = test_peer(&host.transport).await;
        send_device_message(&mut new, 2).await.unwrap();
        let (_, second) = tokio::time::timeout(Duration::from_secs(1), host.messages.recv())
            .await
            .expect("second host message timeout")
            .expect("second host message");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&second).unwrap()["payload"]["sequence"],
            2
        );

        tokio::time::timeout(Duration::from_secs(1), old.connection.closed())
            .await
            .expect("old connection was not closed");
        let _ = send_device_message(&mut old, 3).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), host.messages.recv())
                .await
                .is_err()
        );
        assert!(
            host.transport
                .send_terminal(b"new-owner".to_vec(), Some("device-1"))
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), read_frame(&mut new.terminal_recv))
                .await
                .expect("new owner receive timeout")
                .expect("new owner receive")
                .expect("new owner frame"),
            b"new-owner"
        );
        new.connection.close(0u32.into(), b"test complete");
        old.endpoint.close().await;
        new.endpoint.close().await;
        host.transport.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pairing_side_effect_and_response_share_peer_ownership() {
        let entered = Arc::new(AtomicBool::new(false));
        let entered_for_pairing = Arc::clone(&entered);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let release_for_pairing = Arc::clone(&release_rx);
        let host = test_host_with_pairing(Arc::new(move |handshake| {
            entered_for_pairing.store(true, Ordering::SeqCst);
            release_for_pairing.lock().unwrap().recv().unwrap();
            Some(serde_json::json!({
                "deviceId": handshake.device_id,
                "token": "paired-token",
            }))
        }))
        .await;
        let peer = test_peer(&host.transport).await;
        let peer_key = peer.endpoint.id().to_string();
        let old_id = HOST_PEER_ID.fetch_add(2, Ordering::Relaxed);
        let (control, mut control_rx) = mpsc::channel(1);
        let (terminal, _terminal_rx) = mpsc::channel(1);
        let old = PeerSenders {
            id: old_id,
            connection: peer.connection.clone(),
            control,
            terminal,
            closed: Arc::new(AtomicBool::new(false)),
        };
        host.transport
            .peers
            .lock()
            .unwrap()
            .insert(peer_key.clone(), old.clone());

        let frame = serde_json::to_vec(&serde_json::json!({
            "type": codux_protocol::REMOTE_PAIRING_REQUEST,
            "deviceId": "device-1",
            "requestId": "pairing-1",
            "payload": { "deviceId": "device-1" },
        }))
        .unwrap();
        let transport = Arc::clone(&host.transport);
        let peer_key_for_handler = peer_key.clone();
        let old_for_handler = old.clone();
        let handler = std::thread::spawn(move || {
            transport.handle_frame(
                &peer_key_for_handler,
                &old_for_handler,
                "pending".to_string(),
                frame,
            );
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while !entered.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pairing callback did not start");

        let (replacement_control, _replacement_control_rx) = mpsc::channel(1);
        let (replacement_terminal, _replacement_terminal_rx) = mpsc::channel(1);
        let replacement = PeerSenders {
            id: old_id + 1,
            connection: peer.connection.clone(),
            control: replacement_control,
            terminal: replacement_terminal,
            closed: Arc::new(AtomicBool::new(false)),
        };
        let replacement_done = Arc::new(AtomicBool::new(false));
        let replacement_done_for_thread = Arc::clone(&replacement_done);
        let transport = Arc::clone(&host.transport);
        let replacer = std::thread::spawn(move || {
            let inserted = transport.insert_peer(peer_key, replacement);
            replacement_done_for_thread.store(true, Ordering::SeqCst);
            inserted
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !replacement_done.load(Ordering::SeqCst),
            "replacement crossed pairing's side-effect/publication transaction"
        );
        release_tx.send(()).unwrap();
        handler.join().unwrap();

        let response = control_rx.try_recv().expect("pairing response queued");
        let response: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(response["type"], codux_protocol::REMOTE_PAIRING_CONFIRMED);
        assert_eq!(response["requestId"], "pairing-1");
        assert!(replacer.join().unwrap());

        peer.connection.close(0u32.into(), b"test complete");
        peer.endpoint.close().await;
        host.transport.shutdown().await;
    }

    #[tokio::test]
    async fn full_pairing_response_queue_prevents_pairing_side_effect() {
        let pairing_calls = Arc::new(AtomicU64::new(0));
        let pairing_calls_for_handler = Arc::clone(&pairing_calls);
        let host = test_host_with_pairing(Arc::new(move |_| {
            pairing_calls_for_handler.fetch_add(1, Ordering::SeqCst);
            Some(serde_json::json!({ "token": "must-not-be-written" }))
        }))
        .await;
        let peer = test_peer(&host.transport).await;
        let peer_key = peer.endpoint.id().to_string();
        let (control, _control_rx) = mpsc::channel(1);
        control.try_send(vec![1]).unwrap();
        let (terminal, _terminal_rx) = mpsc::channel(1);
        let senders = PeerSenders {
            id: HOST_PEER_ID.fetch_add(1, Ordering::Relaxed),
            connection: peer.connection.clone(),
            control,
            terminal,
            closed: Arc::new(AtomicBool::new(false)),
        };
        host.transport
            .peers
            .lock()
            .unwrap()
            .insert(peer_key.clone(), senders.clone());
        let frame = serde_json::to_vec(&serde_json::json!({
            "type": codux_protocol::REMOTE_PAIRING_REQUEST,
            "deviceId": "device-1",
            "requestId": "pairing-1",
            "payload": { "deviceId": "device-1" },
        }))
        .unwrap();

        host.transport
            .handle_frame(&peer_key, &senders, "pending".to_string(), frame);

        assert_eq!(pairing_calls.load(Ordering::SeqCst), 0);
        peer.endpoint.close().await;
        host.transport.shutdown().await;
    }

    #[test]
    fn outbound_queue_rejects_excess_without_growing() {
        let (sender, mut receiver) = mpsc::channel::<Vec<u8>>(1);

        assert!(sender.try_send(vec![1]).is_ok());
        assert!(sender.try_send(vec![2]).is_err());
        assert_eq!(receiver.try_recv().unwrap(), vec![1]);
    }

    #[test]
    fn controller_envelope_attaches_current_device_credentials() {
        let credentials = Arc::new(Mutex::new(ControllerCredentials {
            device_id: "device-1".to_string(),
            device_token: "token-1".to_string(),
        }));
        let encoded = authenticated_controller_envelope(
            br#"{"type":"host.info","deviceId":"spoofed","payload":{}}"#.to_vec(),
            &credentials,
        )
        .expect("encoded envelope");
        let envelope: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(envelope["deviceId"], "device-1");
        assert_eq!(envelope["deviceToken"], "token-1");
        assert!(envelope["payload"].as_object().unwrap().is_empty());
    }

    #[test]
    fn pairing_envelope_keeps_credentials_empty() {
        let credentials = Arc::new(Mutex::new(ControllerCredentials::default()));
        let encoded = authenticated_controller_envelope(
            br#"{"type":"pairing.request","deviceId":"pending","payload":{}}"#.to_vec(),
            &credentials,
        )
        .expect("encoded envelope");
        let envelope: serde_json::Value = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(envelope["deviceId"], "pending");
        assert!(envelope.get("deviceToken").is_none());
    }

    #[tokio::test]
    async fn leased_blob_is_collected_after_expiry() {
        let store = new_blob_store_with_gc_interval(Duration::from_millis(10));
        let content = add_blob_with_lease(
            &store,
            b"temporary transfer".to_vec(),
            Duration::from_millis(20),
        )
        .await
        .unwrap();
        assert!(store.blobs().has(content.hash).await.unwrap());

        tokio::time::timeout(Duration::from_secs(1), async {
            while store.blobs().has(content.hash).await.unwrap() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("leased blob should be collected");
    }
}
