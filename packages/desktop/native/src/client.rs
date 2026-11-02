use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use crate::attachments::{SelectedFile, transfer_frame};
use anyhow::{Context, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{
    net::TcpStream,
    sync::{mpsc, watch},
    time,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, client_async_with_config, connect_async_with_config,
    tungstenite::{
        Message,
        error::Error as WsError,
        protocol::frame::coding::CloseCode,
        protocol::{CloseFrame, WebSocketConfig},
    },
};
use uuid::Uuid;

use crate::{
    model::ServerInfo,
    relay::{self, ClientHandshake, FrameKind, PlaintextTraffic, ProtectedChannel},
    wire::{ChunkDecoder, Frame, MAX_MESSAGE_BYTES, TerminalFrame},
};

/// Outbound application frames queue as whole frames; the byte budget bounds
/// what a full frame-count queue can pin in memory.
const COMMAND_QUEUE_BYTE_BUDGET: u64 = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct Target {
    pub url: String,
    auth: Option<Value>,
    transport: TargetTransport,
    /// The daemon machine this target reaches, and whether that machine is this
    /// one. Service URLs and proxy gating read both.
    endpoint: Endpoint,
    /// The daemon home this target was discovered from, so a supervising
    /// connection re-reads `paseo.pid` on every attempt. Explicit endpoints
    /// and saved connections keep `None`.
    home: Option<PathBuf>,
    /// Present only for relay targets: the daemon identity the protected
    /// secure channel authenticates. Credentials never travel on the upgrade.
    pub relay: Option<relay::RelaySecurity>,
}

/// How the WebSocket handshake reaches the daemon. TCP targets (with or
/// without TLS) use [`Target::url`]; local socket and pipe targets carry their
/// path and handshake over that stream with a synthetic `ws://localhost/ws`
/// request, the same shape the shared Node connector uses.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TargetTransport {
    Network,
    Stream { path: PathBuf, pipe: bool },
}

/// The daemon machine as this client reaches it. `local` is true only when the
/// daemon runs on this machine: Paseo's proxy URLs resolve through the
/// daemon's own listener, so a loopback endpoint at the far end of an SSH
/// tunnel cannot serve them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Endpoint {
    host: String,
    local: bool,
}

impl Target {
    pub fn local(home: &Path) -> Result<Self> {
        let state: Value = serde_json::from_slice(
            &fs::read(home.join("paseo.pid")).context("read local daemon discovery")?,
        )?;
        let listen = state["listen"]
            .as_str()
            .context("local daemon discovery has no listener")?;
        let (url, transport) = local_transport(listen)?;
        let auth = read_local_credential(home)?;
        let endpoint = Endpoint {
            host: endpoint_host(&url),
            local: true,
        };
        Ok(Self {
            url,
            auth,
            transport,
            endpoint,
            home: Some(home.into()),
            relay: None,
        })
    }

    pub fn remote(url: String, password: Option<String>) -> Result<Self> {
        ensure!(
            url.starts_with("ws://") || url.starts_with("wss://"),
            "endpoint must use ws:// or wss://"
        );
        Ok(Self {
            url: url.clone(),
            auth: password.map(password_auth),
            transport: TargetTransport::Network,
            endpoint: endpoint_from_url(&url),
            home: None,
            relay: None,
        })
    }

    /// A target reached through a tunnel this client opened. The URL is this
    /// machine's loopback, but the daemon lives on `host`, which is the name a
    /// direct service URL must use.
    pub fn tunneled(url: String, host: String, password: Option<String>) -> Result<Self> {
        ensure!(
            url.starts_with("ws://") || url.starts_with("wss://"),
            "endpoint must use ws:// or wss://"
        );
        ensure!(!host.trim().is_empty(), "tunneled host is required");
        Ok(Self {
            url,
            auth: password.map(password_auth),
            transport: TargetTransport::Network,
            endpoint: Endpoint {
                host: host.trim().to_owned(),
                local: false,
            },
            home: None,
            relay: None,
        })
    }

    /// A relay-carried daemon connection: the upgrade carries routing only,
    /// and the daemon public key from the pairing offer pins the identity the
    /// Noise handshake authenticates. The daemon hello (credentials included)
    /// only leaves after the protected ready verifies.
    pub fn relay(
        url: String,
        daemon_public_key_b64: String,
        password: Option<String>,
    ) -> Result<Self> {
        ensure!(
            url.starts_with("ws://") || url.starts_with("wss://"),
            "endpoint must use ws:// or wss://"
        );
        let relay = relay::RelaySecurity::new(daemon_public_key_b64)
            .context("relay host identity is invalid")?;
        Ok(Self {
            url: url.clone(),
            auth: password.map(|password| json!({"kind":"password","password":password})),
            transport: TargetTransport::Network,
            endpoint: endpoint_from_url(&url),
            home: None,
            relay: Some(relay),
        })
    }

    /// A Unix socket or Windows named pipe target. The local daemon credential
    /// is deliberately not attached here: only discovery through `paseo.pid`
    /// may use it, so a saved or pasted path can never receive it.
    pub fn socket(path: PathBuf, pipe: bool, password: Option<String>) -> Result<Self> {
        ensure!(!path.as_os_str().is_empty(), "socket path is required");
        let url = "ws://localhost/ws".to_owned();
        Ok(Self {
            url: url.clone(),
            auth: password.map(password_auth),
            transport: TargetTransport::Stream {
                path: expand_socket_path(path, pipe),
                pipe,
            },
            endpoint: Endpoint {
                host: endpoint_host(&url),
                local: true,
            },
            home: None,
            relay: None,
        })
    }

    /// True only for the target read from this client's own `paseo.pid`.
    pub fn is_local_discovery(&self) -> bool {
        self.home.is_some()
    }

    pub fn is_stream(&self) -> bool {
        matches!(self.transport, TargetTransport::Stream { .. })
    }
}

fn password_auth(password: String) -> Value {
    json!({"kind":"password","password":password})
}

fn read_local_credential(home: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(home.join("local-credential")) {
        Ok(token) => {
            let token = token.trim();
            Ok((!token.is_empty()).then(|| json!({"kind":"localCredential","token":token})))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context("read local daemon credential"),
    }
}

fn local_transport(listen: &str) -> Result<(String, TargetTransport)> {
    let listen = listen.trim();
    if let Some(path) = listen.strip_prefix("pipe://") {
        return local_stream(path, true);
    }
    if listen.starts_with("\\\\.\\pipe\\") {
        return local_stream(listen, true);
    }
    if let Some(path) = listen.strip_prefix("unix://") {
        return local_stream(path, false);
    }
    if listen.starts_with('/') || listen.starts_with('~') {
        return local_stream(listen, false);
    }
    if is_windows_drive_path(listen) {
        bail!("local daemon listener is a Windows path, not a socket");
    }
    local_tcp_url(listen)
}

fn local_stream(path: &str, pipe: bool) -> Result<(String, TargetTransport)> {
    let path = path.trim();
    ensure!(
        !path.is_empty() && path != "~",
        "local daemon socket path is empty"
    );
    Ok((
        "ws://localhost/ws".into(),
        TargetTransport::Stream {
            path: expand_socket_path(PathBuf::from(path), pipe),
            pipe,
        },
    ))
}

fn local_tcp_url(listen: &str) -> Result<(String, TargetTransport)> {
    let (host, port) = if listen.bytes().all(|byte| byte.is_ascii_digit()) {
        ("127.0.0.1", listen)
    } else if let Some(rest) = listen.strip_prefix('[') {
        let close = rest
            .find(']')
            .context("local daemon listener has no port")?;
        let port = rest[close + 1..]
            .strip_prefix(':')
            .context("local daemon listener has no port")?;
        (&rest[..close], port)
    } else {
        listen
            .rsplit_once(':')
            .context("local daemon listener has no port")?
    };
    let port: u16 = port
        .parse()
        .ok()
        .filter(|port| (1..=65535).contains(port))
        .context("local daemon listener has an invalid port")?;
    ensure!(
        matches!(host, "127.0.0.1" | "localhost" | "0.0.0.0" | "::1" | "::"),
        "local TCP discovery must use loopback"
    );
    Ok((
        format!("ws://localhost:{port}/ws"),
        TargetTransport::Network,
    ))
}

fn is_windows_drive_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn expand_socket_path(path: PathBuf, pipe: bool) -> PathBuf {
    if pipe {
        return path;
    }
    let Ok(rest) = path.strip_prefix("~") else {
        return path;
    };
    let Some(home) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) else {
        return path;
    };
    PathBuf::from(home).join(rest)
}

/// The host part of the endpoint, so a service URL can name the same machine.
fn endpoint_host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        authority.split(':').next().unwrap_or(authority)
    };
    if host.is_empty() {
        "localhost".to_owned()
    } else {
        host.to_owned()
    }
}

/// Whether the endpoint host is this machine. Only loopback counts: a LAN or
/// tunnel host may reach the daemon, but its process-local proxy hostnames do
/// not resolve here.
fn endpoint_is_local(host: &str) -> bool {
    matches!(
        host.trim().to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "::1"
    )
}

fn endpoint_from_url(url: &str) -> Endpoint {
    let host = endpoint_host(url);
    let local = endpoint_is_local(&host);
    Endpoint { host, local }
}

pub fn default_home() -> Result<PathBuf> {
    if let Some(home) = env::var_os("PASEO_HOME") {
        return Ok(home.into());
    }
    let home = env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("home directory is unavailable")?;
    Ok(PathBuf::from(home).join(".paseo"))
}

pub enum Event {
    Ready(ServerInfo),
    Session { kind: String, payload: Value },
    Terminal(TerminalFrame),
    Binary(Vec<u8>),
    Disconnected(String),
}

enum Command {
    Send(u64, Frame),
    Release(u64, String),
}

impl Command {
    /// Bytes this queued command reserved from the connection's queue budget.
    /// Releases reserve nothing — they are tiny control frames — so the ledger
    /// only ever tracks bytes a queued send actually pins.
    fn reserved_bytes(&self) -> u64 {
        match self {
            Command::Send(_, Frame::Text(text)) => text.len() as u64,
            Command::Send(_, Frame::Binary(bytes)) => bytes.len() as u64,
            Command::Release(..) => 0,
        }
    }
}

/// Releases a dequeued command's byte reservation exactly once: on successful
/// delivery (`release`), on a failed send after dequeue, on a stale-epoch drop,
/// and on reconnect drain — every path goes through `Drop` unless explicitly
/// released. Without this guard a send that fails mid-`.await` would leave its
/// reservation occupied forever.
struct QueueReservation<'a> {
    ledger: &'a AtomicU64,
    bytes: u64,
}

impl QueueReservation<'_> {
    /// Consuming the guard returns its reservation immediately; Drop also
    /// covers failure, cancellation and stale-epoch discard paths.
    fn release(self) {
        drop(self);
    }
}

impl Drop for QueueReservation<'_> {
    fn drop(&mut self) {
        if self.bytes != 0 {
            self.ledger.fetch_sub(self.bytes, Ordering::SeqCst);
        }
    }
}

pub struct Connection {
    subscriptions: Arc<Mutex<crate::subscriptions::Subscriptions>>,
    commands: mpsc::Sender<Command>,
    uploads: mpsc::Sender<(u64, Frame)>,
    ready: Arc<AtomicU64>,
    upload_state: watch::Sender<u64>,
    shutdown: watch::Sender<bool>,
    endpoint: Endpoint,
    queued_bytes: Arc<AtomicU64>,
    /// Whether this connection dials a relay: relay wire limits are tighter
    /// than the daemon's and are enforced at enqueue, before any queueing.
    relay_transport: bool,
    /// Mirrors the negotiated `binaryChunkingV1` of the live attempt so the
    /// enqueue gate knows whether oversized relay frames can be outer-chunked.
    chunking_supported: Arc<AtomicBool>,
    /// Once true, this process has completed a protected relay handshake. The
    /// native client requires protected on every attempt anyway, so this is
    /// the process floor and the learned mode a host can persist on adopt.
    protected_established: Arc<AtomicBool>,
    pub events: Receiver<Event>,
}

impl Connection {
    pub fn start(target: Target) -> Result<Self> {
        let endpoint = target.endpoint.clone();
        let subscriptions = Arc::new(Mutex::new(crate::subscriptions::Subscriptions::default()));
        let network_subscriptions = subscriptions.clone();
        let (commands, receiver) = mpsc::channel(256);
        // Bulk transfers have two bounded frames and yield to interactive input and replies.
        let (uploads, upload_receiver) = mpsc::channel(2);
        let (shutdown, mut shutdown_receiver) = watch::channel(false);
        let ready = Arc::new(AtomicU64::new(0));
        let generation = Arc::new(AtomicU64::new(0));
        let queued_bytes = Arc::new(AtomicU64::new(0));
        let chunking_supported = Arc::new(AtomicBool::new(false));
        let relay_transport = target.relay.is_some();
        let protected_established = Arc::new(AtomicBool::new(false));
        let network_ready = ready.clone();
        let network_generation = generation.clone();
        let network_queued_bytes = queued_bytes.clone();
        let network_chunking = chunking_supported.clone();
        let network_protected = protected_established.clone();
        let (upload_state, _) = watch::channel(0);
        let network_upload_state = upload_state.clone();
        let (events, event_receiver) = async_channel::bounded(4);
        std::thread::Builder::new()
            .name("paseo-connection".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("start network runtime")
                    .and_then(|runtime| {
                        runtime.block_on(async {tokio::select!{result=supervise(target, receiver, upload_receiver, &events, network_subscriptions, network_ready, network_generation, network_upload_state, network_queued_bytes, network_chunking, network_protected)=>result,_=shutdown_receiver.changed()=>Ok(())}})
                    });
                if let Err(error) = result {
                    let _ = events.send_blocking(Event::Disconnected(format!("{error:#}")));
                }
            })?;
        Ok(Self {
            subscriptions,
            commands,
            uploads,
            ready,
            upload_state,
            shutdown,
            endpoint,
            queued_bytes,
            relay_transport,
            chunking_supported,
            protected_established,
            events: event_receiver,
        })
    }

    /// The daemon machine, without scheme or port, for direct service URLs.
    pub fn host(&self) -> &str {
        &self.endpoint.host
    }

    /// Whether the endpoint names this machine. Paseo's local proxy URLs only
    /// resolve where the daemon runs, so a remote or tunneled connection
    /// cannot open them.
    pub fn is_local_endpoint(&self) -> bool {
        self.endpoint.local
    }

    /// The endpoint security negotiated on this connection's relay transport.
    /// `Some("protected")` once a protected handshake has completed; the host
    /// runtime can persist this when adopting the host. Direct and local
    /// targets never negotiate a relay mode and stay `None`.
    pub fn negotiated_security(&self) -> Option<&'static str> {
        self.protected_established
            .load(Ordering::SeqCst)
            .then_some("protected")
    }

    pub fn connected(&self) -> bool {
        self.ready.load(Ordering::SeqCst) != 0
    }

    pub fn close(&self) {
        self.ready.store(0, Ordering::SeqCst);
        self.upload_state.send_replace(0);
        let _ = self.shutdown.send(true);
    }

    pub fn request(&self, kind: &str, mut fields: Value) -> Result<String> {
        let request_id = Uuid::new_v4().to_string();
        let object = fields
            .as_object_mut()
            .context("request fields must be an object")?;
        object.insert("type".into(), kind.into());
        object.insert("requestId".into(), request_id.clone().into());
        self.send_session(fields)?;
        Ok(request_id)
    }

    pub fn subscribe(&self, key: &str, kind: &str, fields: Value) -> Result<String> {
        let mut subscriptions = self.subscriptions.lock().expect("subscription lock");
        subscriptions.can_open()?;
        let request = self.request(kind, fields)?;
        subscriptions.open(key, request.clone());
        Ok(request)
    }

    pub fn unsubscribe(&self, key: &str) {
        let epoch = self.ready.load(Ordering::SeqCst);
        let mut subscriptions = self.subscriptions.lock().expect("subscription lock");
        if let Some(id) = subscriptions.close(key)
            && let Err(error) = self.commands.try_send(Command::Release(epoch, id))
            && let Command::Release(_, id) = error.into_inner()
        {
            // Drain already-queued input before releasing its slot. A full
            // command queue already wakes the network loop.
            subscriptions.defer_release(id);
        }
    }

    pub fn send_session(&self, message: Value) -> Result<()> {
        self.send_session_data(&message)
    }

    pub fn send_session_data<T: Serialize>(&self, message: &T) -> Result<()> {
        #[derive(Serialize)]
        struct Envelope<'a, T> {
            #[serde(rename = "type")]
            kind: &'static str,
            message: &'a T,
        }
        let mut buffer = BoundedJson::new(MAX_MESSAGE_BYTES);
        serde_json::to_writer(
            &mut buffer,
            &Envelope {
                kind: "session",
                message,
            },
        )?;
        self.send(Frame::Text(String::from_utf8(buffer.bytes)?))
    }

    async fn send_upload(&self, generation: u64, frame: Frame) -> Result<()> {
        let mut state = self.upload_state.subscribe();
        ensure!(
            *state.borrow() == generation && generation != 0,
            "Host disconnected during upload"
        );
        tokio::select! {
            result = self.uploads.send((generation, frame)) => result.context("Host disconnected during upload"),
            _ = state.changed() => bail!("Host disconnected during upload"),
        }
    }

    pub async fn upload_file(
        &self,
        mut file: SelectedFile,
        request_id: &str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<()> {
        let metadata = json!({"mime":file.mime_type,"size":file.size,"encoding":"binary","modifiedAt":file.modified_at,"fileName":file.file_name});
        let request = json!({"type":"file.upload.request","requestId":request_id,"fileName":file.file_name,"mimeType":file.mime_type,"size":file.size,"modifiedAt":file.modified_at});
        ensure!(!cancelled.load(Ordering::Relaxed), "Upload cancelled");
        let generation = self.ready.load(Ordering::SeqCst);
        ensure!(generation != 0, "Host disconnected during upload");
        // Keep the request and file frames on the same FIFO; interactive commands can overtake
        // the bulk queue without letting a FileBegin overtake its own registration request.
        self.send_upload(
            generation,
            Frame::Text(serde_json::to_string(
                &json!({"type":"session","message":request}),
            )?),
        )
        .await?;
        self.send_upload(
            generation,
            Frame::Binary(transfer_frame(
                0x10,
                request_id,
                &serde_json::to_vec(&metadata)?,
            )?),
        )
        .await?;
        let result: Result<()> = async {
            let mut buffer = vec![0; 128 * 1024];
            let mut total = 0;
            loop {
                ensure!(!cancelled.load(Ordering::Relaxed), "Upload cancelled");
                ensure!(
                    self.ready.load(Ordering::SeqCst) == generation,
                    "Host disconnected during upload"
                );
                let len = file
                    .file
                    .read(&mut buffer)
                    .context("Cannot read selected file")?;
                if len == 0 {
                    break;
                }
                total += len as u64;
                ensure!(total <= file.size, "File changed during upload");
                self.send_upload(
                    generation,
                    Frame::Binary(transfer_frame(0x11, request_id, &buffer[..len])?),
                )
                .await?;
            }
            ensure!(total == file.size, "File changed during upload");
            Ok(())
        }
        .await;
        // The protocol has no abort frame. End an interrupted partial transfer so the host
        // rejects its size and reclaims it instead of waiting for the stale-upload timer.
        ensure!(
            self.ready.load(Ordering::SeqCst) == generation,
            "Host disconnected during upload"
        );
        let end = self
            .send_upload(
                generation,
                Frame::Binary(transfer_frame(0x12, request_id, &[])?),
            )
            .await;
        result.and(end)
    }

    pub fn send_terminal(&self, frame: TerminalFrame) -> Result<()> {
        self.send(Frame::Binary(frame.encode()))
    }

    fn send(&self, frame: Frame) -> Result<()> {
        let len = match &frame {
            Frame::Text(s) => s.len(),
            Frame::Binary(b) => b.len(),
        };
        ensure!(len <= MAX_MESSAGE_BYTES, "logical message too large");
        // A relay frame that cannot fit one protected record in one relay
        // WebSocket message needs outer chunking (binaryChunkingV1). Fail the
        // request here — at accept — with an actionable reason instead of
        // reporting a successful enqueue and then dropping the connection.
        if self.relay_transport
            && len > relay::max_relay_frame_bytes(relay::MAX_RELAY_WEBSOCKET_MESSAGE_BYTES)
            && !self.chunking_supported.load(Ordering::SeqCst)
        {
            bail!(
                "This host cannot carry a message this large over the relay: it does not support binaryChunkingV1. Update the host daemon, or send a smaller message."
            );
        }
        let generation = self.ready.load(Ordering::SeqCst);
        ensure!(
            generation != 0,
            "Host is disconnected; input was not queued"
        );
        // Bound queued bytes, not just frame count: a full frame-count queue of
        // large frames must fail fast instead of pinning unbounded memory.
        let queued = self.queued_bytes.fetch_add(len as u64, Ordering::SeqCst) + len as u64;
        if queued > COMMAND_QUEUE_BYTE_BUDGET {
            self.queued_bytes.fetch_sub(len as u64, Ordering::SeqCst);
            bail!("connection input queue is full");
        }
        if let Err(error) = self.commands.try_send(Command::Send(generation, frame)) {
            self.queued_bytes.fetch_sub(len as u64, Ordering::SeqCst);
            return Err(error).context("connection input queue is unavailable");
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.close();
    }
}

// A large image message must fail at the wire limit before allocating an unbounded JSON
// string. Serialize once into the bounded buffer; retries still retain the original draft.
struct BoundedJson {
    bytes: Vec<u8>,
    limit: usize,
}
impl BoundedJson {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(limit.min(1024)),
            limit,
        }
    }
}
impl std::io::Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|next| *next <= self.limit)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Message is too large. Remove attachments or shorten the message.",
                )
            })?;
        if next > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(next)
                .min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// The concrete WebSocket streams the client dials. TCP keeps the TLS-capable
/// connector; Unix sockets and Windows named pipes hand-shake over that local
/// stream with the same WebSocket protocol.
enum HostSocket {
    Network(Socket),
    #[cfg(unix)]
    Unix(WebSocketStream<tokio::net::UnixStream>),
    #[cfg(windows)]
    Pipe(WebSocketStream<tokio::net::windows::named_pipe::NamedPipeClient>),
}

impl HostSocket {
    async fn send(&mut self, message: Message) -> std::result::Result<(), WsError> {
        match self {
            HostSocket::Network(socket) => socket.send(message).await,
            #[cfg(unix)]
            HostSocket::Unix(socket) => socket.send(message).await,
            #[cfg(windows)]
            HostSocket::Pipe(socket) => socket.send(message).await,
        }
    }

    async fn next(&mut self) -> Option<std::result::Result<Message, WsError>> {
        match self {
            HostSocket::Network(socket) => socket.next().await,
            #[cfg(unix)]
            HostSocket::Unix(socket) => socket.next().await,
            #[cfg(windows)]
            HostSocket::Pipe(socket) => socket.next().await,
        }
    }

    async fn close(&mut self, frame: Option<CloseFrame<'_>>) -> std::result::Result<(), WsError> {
        match self {
            HostSocket::Network(socket) => socket.close(frame).await,
            #[cfg(unix)]
            HostSocket::Unix(socket) => socket.close(frame).await,
            #[cfg(windows)]
            HostSocket::Pipe(socket) => socket.close(frame).await,
        }
    }
}

/// Opens the transport the target names. Local socket paths never fall back to
/// TCP, so a missing socket reports a real dial failure instead of connecting
/// somewhere else.
async fn dial(target: &Target) -> Result<HostSocket> {
    // The relay's own 32 MiB message cap is the receive bound on a protected
    // channel, not the daemon's 100 MiB.
    let wire_bound = if target.relay.is_some() {
        relay::MAX_RELAY_WEBSOCKET_MESSAGE_BYTES
    } else {
        MAX_MESSAGE_BYTES
    };
    let config = WebSocketConfig {
        max_message_size: Some(wire_bound),
        max_frame_size: Some(wire_bound),
        ..Default::default()
    };
    match &target.transport {
        TargetTransport::Network => {
            let (socket, _) = time::timeout(
                CONNECT_TIMEOUT,
                connect_async_with_config(&target.url, Some(config), false),
            )
            .await
            .context("daemon connection timed out")??;
            Ok(HostSocket::Network(socket))
        }
        TargetTransport::Stream { path, pipe } => Ok(time::timeout(CONNECT_TIMEOUT, async {
            #[cfg(unix)]
            {
                ensure!(
                    !pipe,
                    "Windows named pipe hosts can only be used on Windows"
                );
                let stream = tokio::net::UnixStream::connect(path)
                    .await
                    .with_context(|| format!("connect to local Unix socket {}", path.display()))?;
                let (socket, _) =
                    client_async_with_config("ws://localhost/ws", stream, Some(config)).await?;
                Ok(HostSocket::Unix(socket))
            }
            #[cfg(windows)]
            {
                ensure!(pipe, "Unix socket hosts can only be used on Unix");
                let stream = tokio::net::windows::named_pipe::ClientOptions::new()
                    .open(path.as_os_str())
                    .with_context(|| format!("connect to local named pipe {}", path.display()))?;
                let (socket, _) =
                    client_async_with_config("ws://localhost/ws", stream, Some(config)).await?;
                Ok(HostSocket::Pipe(socket))
            }
            #[cfg(not(any(unix, windows)))]
            {
                let _ = (path, pipe, config);
                anyhow::bail!("Local socket hosts are not supported on this platform")
            }
        })
        .await
        .context("daemon connection timed out")??),
    }
}

const DIRECT_CHUNK_BYTES: usize = 1024 * 1024;

/// Best-effort wire close carrying the structured transport-security close
/// code (4002 handshake / 4003 record), bounded so a stuck relay never delays
/// the error report. Non-structured errors close implicitly on socket drop.
async fn close_structured(socket: &mut HostSocket, error: &anyhow::Error) {
    let Some(failure) = error.downcast_ref::<relay::TransportSecurityError>() else {
        return;
    };
    let frame = CloseFrame {
        code: CloseCode::Bad(failure.close_code),
        reason: failure.reason.into(),
    };
    let _ = time::timeout(Duration::from_secs(1), socket.close(Some(frame))).await;
}

/// Delivers one application frame. Over a protected relay channel the frame
/// becomes one record in one WebSocket message (base64 text or binary as
/// negotiated); the outer binaryChunkingV1 layer stays a separate layer above
/// the records, chunking frames that exceed the relay record bound.
async fn send(
    socket: &mut HostSocket,
    mut secure: Option<&mut ProtectedChannel>,
    frame: Frame,
    chunked: bool,
) -> Result<()> {
    // Enforce the relay record bound before any record encoding: an oversized
    // unchunked protected frame must fail this request instead of consuming a
    // Noise send nonce and only then being rejected by the relay (which drops
    // the whole channel).
    let frame_len = match &frame {
        Frame::Text(text) => text.len(),
        Frame::Binary(bytes) => bytes.len(),
    };
    if secure.is_some()
        && !chunked
        && frame_len > relay::max_relay_frame_bytes(relay::MAX_RELAY_WEBSOCKET_MESSAGE_BYTES)
    {
        bail!(
            "relay frame exceeds the protected single-message limit and binaryChunkingV1 is not negotiated"
        );
    }
    let (kind, bytes) = match frame {
        Frame::Text(text) if !chunked || text.len() <= outer_chunk_payload(secure.is_some()) => {
            return deliver(socket, secure, FrameKind::Text, text.into_bytes()).await;
        }
        Frame::Binary(bytes)
            if !chunked || bytes.len() <= outer_chunk_payload(secure.is_some()) =>
        {
            return deliver(socket, secure, FrameKind::Binary, bytes).await;
        }
        Frame::Text(text) => (0u8, text.into_bytes()),
        Frame::Binary(bytes) => (1u8, bytes),
    };
    let payload = outer_chunk_payload(secure.is_some());
    let count = bytes.len().div_ceil(payload);
    for (index, chunk) in bytes.chunks(payload).enumerate() {
        let flags = u8::from(index == 0) | (u8::from(index + 1 == count) << 1);
        let mut frame = Vec::with_capacity(chunk.len() + 4);
        frame.extend_from_slice(&[0x31, 1, kind, flags]);
        frame.extend_from_slice(chunk);
        deliver(socket, secure.as_deref_mut(), FrameKind::Binary, frame).await?;
    }
    Ok(())
}

/// The outer chunking threshold: the direct wire uses 1 MiB chunks; the relay
/// wire chunks at the largest frame whose protected record still fits one
/// relay WebSocket message including base64 overhead.
fn outer_chunk_payload(secure: bool) -> usize {
    if secure {
        relay::max_relay_frame_bytes(relay::MAX_RELAY_WEBSOCKET_MESSAGE_BYTES)
    } else {
        DIRECT_CHUNK_BYTES
    }
}

async fn deliver(
    socket: &mut HostSocket,
    secure: Option<&mut ProtectedChannel>,
    kind: FrameKind,
    bytes: Vec<u8>,
) -> Result<()> {
    match secure {
        None => match kind {
            FrameKind::Text => {
                socket
                    .send(Message::Text(String::from_utf8(bytes)?))
                    .await?
            }
            FrameKind::Binary => socket.send(Message::Binary(bytes)).await?,
        },
        Some(channel) => {
            let record = channel.encode_frame(kind, &bytes)?;
            if kind == FrameKind::Binary && channel.binary_ciphertext() {
                socket.send(Message::Binary(record)).await?;
            } else {
                let text = base64::engine::general_purpose::STANDARD.encode(record);
                socket.send(Message::Text(text)).await?;
            }
        }
    }
    Ok(())
}

/// Decodes one inbound WebSocket message on an open protected channel into an
/// application frame. Handshake artifacts (retried hellos, duplicate readies)
/// are ignored; any other plaintext is a peer that stopped encrypting, and any
/// record failure is fatal for the channel (close 4003). Ping/Pong/Close are
/// handled by the caller.
fn secure_receive(
    channel: &mut ProtectedChannel,
    message: Message,
    chunks: &mut ChunkDecoder,
) -> Result<Option<Frame>> {
    let record = match message {
        Message::Text(text) => match relay::classify_plaintext_text(&text) {
            PlaintextTraffic::HandshakeArtifact => return Ok(None),
            PlaintextTraffic::PlaintextApplication => {
                bail!(relay::TransportSecurityError::record(
                    "received plaintext frame on encrypted channel"
                ));
            }
            PlaintextTraffic::RecordBase64 => {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD
                    .decode(text.as_bytes())
                    .context(relay::TransportSecurityError::record(
                        "invalid record encoding",
                    ))?
            }
        },
        Message::Binary(bytes) => bytes,
        _ => return Ok(None),
    };
    let decoded = channel.decode_record(&record)?;
    let frame =
        match decoded.kind {
            FrameKind::Text => Frame::Text(String::from_utf8(decoded.data).map_err(|_| {
                relay::TransportSecurityError::record("invalid text frame encoding")
            })?),
            FrameKind::Binary => Frame::Binary(decoded.data),
        };
    chunks.receive(frame)
}

async fn release_subscription(
    socket: &mut HostSocket,
    secure: Option<&mut ProtectedChannel>,
    id: String,
    chunked: bool,
) -> Result<()> {
    send(
        socket,
        secure,
        Frame::Text(
            json!({"type":"session","message":{"type":"subscription.release.request","requestId":Uuid::new_v4().to_string(),"subscriptionId":id}})
                .to_string(),
        ),
        chunked,
    )
    .await
}

async fn supervise(
    target: Target,
    mut commands: mpsc::Receiver<Command>,
    mut uploads: mpsc::Receiver<(u64, Frame)>,
    events: &Sender<Event>,
    subscriptions: Arc<Mutex<crate::subscriptions::Subscriptions>>,
    ready: Arc<AtomicU64>,
    generation: Arc<AtomicU64>,
    upload_state: watch::Sender<u64>,
    queued_bytes: Arc<AtomicU64>,
    chunking_supported: Arc<AtomicBool>,
    protected_established: Arc<AtomicBool>,
) -> Result<()> {
    let mut delay = 250;
    let mut server_id = None;
    loop {
        let epoch = generation.fetch_add(1, Ordering::SeqCst) + 1;
        let target = match &target.home {
            Some(home) => Target::local(home),
            None => Ok(target.clone()),
        };
        let result = match target {
            Ok(target) => {
                run(
                    target,
                    &mut commands,
                    &mut uploads,
                    events,
                    subscriptions.clone(),
                    &ready,
                    &upload_state,
                    epoch,
                    &mut server_id,
                    &queued_bytes,
                    &chunking_supported,
                    &protected_established,
                )
                .await
            }
            Err(error) => Err(error),
        };
        ready.store(0, Ordering::SeqCst);
        upload_state.send_replace(0);
        subscriptions.lock().expect("subscription lock").reset();
        while let Ok(command) = commands.try_recv() {
            queued_bytes.fetch_sub(command.reserved_bytes(), Ordering::SeqCst);
        }
        while uploads.try_recv().is_ok() {}
        let Err(error) = result else {
            return Ok(());
        };
        // Structured transport-security failures carry their own close code:
        // a 4002 means this host cannot do protected relay security, so
        // retrying cannot help. String matching stays only for the legacy
        // direct-connection fatal paths.
        let security_fatal = error
            .downcast_ref::<relay::TransportSecurityError>()
            .is_some_and(|failure| {
                failure.close_code == relay::PROTECTED_HANDSHAKE_FAILED_CLOSE_CODE
            });
        let fatal = security_fatal
            || error.to_string().contains("rejected authentication")
            || error.to_string().contains("different host")
            || error.to_string().contains("invalid daemon envelope")
            || error.to_string().contains("invalid server info")
            || error.to_string().contains("session message has no type");
        events
            .send(Event::Disconnected(match security_fatal {
                // A 4002 means this host cannot do protected relay security at
                // all: say what fixes it instead of surfacing a bare close code.
                true => format!(
                    "This host does not support protected relay security. Update the host daemon and relay, then reconnect. ({error:#})"
                ),
                false => format!("{error:#}"),
            }))
            .await?;
        if fatal {
            return Ok(());
        }
        time::sleep(Duration::from_millis(delay)).await;
        delay = (delay * 2).min(5000);
    }
}

async fn run(
    target: Target,
    commands: &mut mpsc::Receiver<Command>,
    uploads: &mut mpsc::Receiver<(u64, Frame)>,
    events: &Sender<Event>,
    subscriptions: Arc<Mutex<crate::subscriptions::Subscriptions>>,
    ready_state: &AtomicU64,
    upload_state: &watch::Sender<u64>,
    generation: u64,
    server_id: &mut Option<String>,
    queued_bytes: &AtomicU64,
    chunking_supported: &AtomicBool,
    protected_established: &AtomicBool,
) -> Result<()> {
    let mut socket = dial(&target).await?;
    // Relay upgrades carry routing only. The daemon hello (credentials
    // included) leaves only after the protected ready authenticates; direct
    // and local targets send it immediately as before.
    let mut channel = match &target.relay {
        Some(relay) => {
            let handshake = ClientHandshake::begin(&relay.daemon_public_key_b64)
                .context("relay security handshake setup")?;
            match open_protected_channel(&mut socket, handshake).await {
                Ok(channel) => Some(channel),
                Err(error) => {
                    close_structured(&mut socket, &error).await;
                    return Err(error);
                }
            }
        }
        None => None,
    };
    if channel.is_some() {
        protected_established.store(true, Ordering::SeqCst);
    }
    let mut hello = json!({"type":"hello","clientId":Uuid::new_v4().to_string(),"clientType":"browser","protocolVersion":1,"appVersion":env!("CARGO_PKG_VERSION"),"capabilities":{
        "hello_rejection":true,"explicit_event_subscriptions":true,"all_providers":true,"selective_agent_timeline":true,"terminal_chunked_replay":true,"binary_chunking_v1":true,"project_updates":true,"timeline_replacement_invalidation":true,"owned_subscriptions":true
    }});
    if let Some(auth) = target.auth {
        hello["auth"] = auth;
    }
    send(
        &mut socket,
        channel.as_mut(),
        Frame::Text(hello.to_string()),
        false,
    )
    .await?;
    let mut chunks = ChunkDecoder::default();
    let mut chunked = false;
    let mut ready = false;
    let mut bulk_turn = false;
    let mut heartbeat = time::interval(Duration::from_secs(15));
    let handshake_deadline = time::Instant::now() + Duration::from_secs(15);
    let mut last_received = time::Instant::now();
    heartbeat.tick().await;
    loop {
        if let Ok(command) = commands.try_recv() {
            let reservation = QueueReservation {
                ledger: queued_bytes,
                bytes: command.reserved_bytes(),
            };
            match command {
                Command::Send(epoch, frame) if ready && epoch == generation => {
                    send(&mut socket, channel.as_mut(), frame, chunked).await?
                }
                Command::Release(epoch, id) if ready && epoch == generation => {
                    release_subscription(&mut socket, channel.as_mut(), id, chunked).await?
                }
                _ => {}
            }
            reservation.release();
            continue;
        }
        let release = subscriptions
            .lock()
            .expect("subscription lock")
            .next_release();
        if let Some(id) = release {
            release_subscription(&mut socket, channel.as_mut(), id, chunked).await?;
            continue;
        }
        // Give bulk data a turn between inbound records, even while a terminal streams.
        if ready
            && bulk_turn
            && let Ok((epoch, frame)) = uploads.try_recv()
        {
            bulk_turn = false;
            if epoch == generation {
                send(&mut socket, channel.as_mut(), frame, chunked).await?;
            }
            continue;
        }
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(command) => {
                    let reservation = QueueReservation {
                        ledger: queued_bytes,
                        bytes: command.reserved_bytes(),
                    };
                    match command {
                        Command::Send(epoch, frame) if ready && epoch == generation => send(&mut socket, channel.as_mut(), frame, chunked).await?,
                        Command::Release(epoch, id) if ready && epoch == generation => release_subscription(&mut socket, channel.as_mut(), id, chunked).await?,
                        _ => {},
                    }
                    reservation.release();
                }
                None => { socket.close(None).await?; return Ok(()); }
            },
            _ = time::sleep_until(handshake_deadline), if !ready => bail!("daemon handshake timed out"),
            _ = heartbeat.tick() => {
                ensure!(last_received.elapsed() < Duration::from_secs(45), "daemon heartbeat timed out");
                send(&mut socket, channel.as_mut(), Frame::Text(json!({"type":"ping"}).to_string()), chunked).await?;
            }
            message = socket.next() => {
                last_received = time::Instant::now();
                bulk_turn=true;
                let decoded: Result<Option<Frame>> = match message.context("daemon disconnected")?? {
                    Message::Text(text) => {
                        match channel.as_mut() {
                            Some(channel) => secure_receive(channel, Message::Text(text), &mut chunks),
                            None => chunks.receive(Frame::Text(text)),
                        }
                    }
                    Message::Binary(bytes) => {
                        match channel.as_mut() {
                            Some(channel) => secure_receive(channel, Message::Binary(bytes), &mut chunks),
                            None => chunks.receive(Frame::Binary(bytes)),
                        }
                    }
                    Message::Ping(bytes) => { socket.send(Message::Pong(bytes)).await?; continue; }
                    Message::Pong(_) => continue,
                    Message::Close(_) => bail!("daemon closed connection"),
                    Message::Frame(_) => continue,
                };
                let frame = match decoded {
                    Ok(frame) => frame,
                    Err(error) => {
                        // A record failure is a structured 4003: tell the peer
                        // why we are closing before the socket drops.
                        close_structured(&mut socket, &error).await;
                        return Err(error);
                    }
                };
                let Some(frame) = frame else { continue; };
                match frame {
                    Frame::Binary(bytes) => {
                        if bytes.first().is_some_and(|op| (1..=8).contains(op)) {
                            events.send(Event::Terminal(TerminalFrame::decode(bytes)?)).await?;
                        } else if bytes.first().is_some_and(|op| (0x10..=0x12).contains(op)) {
                            events.send(Event::Binary(bytes)).await?;
                        }
                    }
                    Frame::Text(text) => {
                        let envelope: Value = serde_json::from_str(&text).context("invalid daemon envelope")?;
                        if envelope["type"] == "hello.rejected" { bail!("daemon rejected authentication or protocol version"); }
                        if envelope["type"] != "session" { continue; }
                        let message = &envelope["message"];
                        let kind = message["type"].as_str().context("session message has no type")?;
                        let payload = message["payload"].clone();
                        if kind == "status" && payload["status"] == "server_info" {
                            let info: ServerInfo = serde_json::from_value(payload).context("invalid server info")?;
                            chunked = info.features.get("binaryChunkingV1") == Some(&Value::Bool(true));
                            chunking_supported.store(chunked, Ordering::SeqCst);
                            if let Some(expected) = server_id.as_ref() { ensure!(expected == &info.server_id, "Endpoint resolved to a different host; reconnect explicitly"); }
                            else { *server_id = Some(info.server_id.clone()); }
                            ready = true;
                            ready_state.store(generation, Ordering::SeqCst);
                            upload_state.send_replace(generation);
                            events.send(Event::Ready(info)).await?;
                        } else if ready {
                            let accepted = kind != "subscription.release.response" && subscriptions.lock().expect("subscription lock").receive(&payload);
                            if accepted { events.send(Event::Session { kind: kind.into(), payload }).await?; }
                        }
                    }
                }
            },
            Some((epoch, frame)) = uploads.recv(), if ready => {
                if epoch == generation { send(&mut socket, channel.as_mut(), frame, chunked).await?; }
            },
        }
    }
}

/// Runs the protected handshake on a freshly connected relay socket: the
/// exact cached hello is resent every second until a ready authenticates the
/// daemon. A legacy ready, an invalid protected message, or an unoffered
/// selection is a fatal 4002; every failure is returned as a structured
/// transport-security error so the supervisor stops retrying on 4002 only.
async fn open_protected_channel(
    socket: &mut HostSocket,
    mut handshake: ClientHandshake,
) -> Result<ProtectedChannel> {
    let deadline = time::Instant::now() + Duration::from_secs(15);
    let mut resend = time::interval(Duration::from_secs(1));
    resend.tick().await; // the first tick completes immediately
    let hello_text = handshake.hello_text().to_owned();
    socket
        .send(Message::Text(hello_text.clone()))
        .await
        .context("send the relay security hello")?;
    loop {
        tokio::select! {
            _ = time::sleep_until(deadline) => bail!("relay security handshake timed out"),
            _ = resend.tick() => {
                socket
                    .send(Message::Text(hello_text.clone()))
                    .await
                    .context("resend the relay security hello")?;
            }
            message = socket.next() => {
                match message.context("relay closed during security handshake")?? {
                    Message::Text(text) => match handshake.accept_ready(&text) {
                        Ok(Some(channel)) => return Ok(channel),
                        Ok(None) => continue,
                        Err(failure) => return Err(failure.into()),
                    },
                    Message::Binary(_) => continue,
                    Message::Ping(bytes) => { socket.send(Message::Pong(bytes)).await?; }
                    Message::Pong(_) => {}
                    Message::Close(_) => bail!("relay closed connection during security handshake"),
                    Message::Frame(_) => continue,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_discovery_follows_the_paseo_pid_listen_grammar() {
        let home = std::env::temp_dir().join(format!(
            "paseo-native-discovery-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("local-credential"), "local-token\n").unwrap();

        std::fs::write(home.join("paseo.pid"), r#"{"listen":"/tmp/paseo.sock"}"#).unwrap();
        let target = Target::local(&home).unwrap();
        assert!(target.is_local_discovery());
        assert!(target.is_stream());
        assert_eq!(target.url, "ws://localhost/ws");
        assert!(
            target.auth.is_some(),
            "local socket keeps the local credential"
        );

        std::fs::write(home.join("paseo.pid"), r#"{"listen":"0.0.0.0:4321"}"#).unwrap();
        let target = Target::local(&home).unwrap();
        assert!(!target.is_stream());
        assert_eq!(target.url, "ws://localhost:4321/ws");

        std::fs::write(home.join("paseo.pid"), r#"{"listen":"10.0.0.2:4321"}"#).unwrap();
        assert!(Target::local(&home).is_err());

        std::fs::write(
            home.join("paseo.pid"),
            r#"{"listen":"pipe://\\\\.\\pipe\\paseo"}"#,
        )
        .unwrap();
        assert!(Target::local(&home).unwrap().is_stream());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn saved_and_explicit_targets_never_receive_the_local_credential() {
        let socket = Target::socket(PathBuf::from("/tmp/paseo.sock"), false, None).unwrap();
        assert!(socket.auth.is_none());
        assert!(!socket.is_local_discovery());
        let remote = Target::remote("ws://example.com:6767/ws".into(), None).unwrap();
        assert!(remote.auth.is_none());
        assert!(!remote.is_local_discovery());
    }

    #[test]
    fn service_targets_name_the_daemon_machine_and_gate_local_proxy_urls() {
        assert_eq!(endpoint_host("wss://[::1]:7000/ws"), "::1");
        assert_eq!(endpoint_host("ws://user:pw@10.0.0.5:7000/ws"), "10.0.0.5");
        assert_eq!(endpoint_host("ws://localhost/ws"), "localhost");
        assert!(endpoint_is_local("localhost"));
        assert!(endpoint_is_local("127.0.0.1"));
        assert!(endpoint_is_local("::1"));
        assert!(!endpoint_is_local("10.0.0.5"));
        assert!(!endpoint_is_local("dev.example.net"));

        let local = Target::remote("ws://127.0.0.1:6767/ws".into(), None).unwrap();
        let connection_endpoint = local.endpoint.clone();
        assert_eq!(connection_endpoint.host, "127.0.0.1");
        assert!(connection_endpoint.local);

        // An SSH tunnel dials this machine's loopback, but the daemon is the
        // remote host: direct URLs name it and proxy URLs stay withheld.
        let tunneled = Target::tunneled(
            "ws://127.0.0.1:49999/ws".into(),
            "dev.example.net".into(),
            None,
        )
        .unwrap();
        assert_eq!(tunneled.endpoint.host, "dev.example.net");
        assert!(!tunneled.endpoint.local);
        assert!(
            Target::tunneled("ws://127.0.0.1:49999/ws".into(), "  ".into(), None).is_err(),
            "a tunneled target needs the host it tunnels to"
        );
    }

    #[test]
    fn a_local_discovery_target_is_this_machine() {
        let home = std::env::temp_dir().join(format!(
            "paseo-native-endpoint-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("paseo.pid"), r#"{"listen":"127.0.0.1:6767"}"#).unwrap();
        let target = Target::local(&home).unwrap();
        assert_eq!(target.endpoint.host, "localhost");
        assert!(target.endpoint.local);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_unix_socket_target_completes_a_real_websocket_handshake() {
        let directory = PathBuf::from("/tmp").join(format!(
            "paseo-native-unix-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let hello = socket.next().await.unwrap().unwrap();
            assert!(hello.to_text().unwrap().contains("hello"));
            socket
                .send(Message::Text(
                    json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":"socket-fixture","features":{}}}})
                        .to_string(),
                ))
                .await
                .unwrap();
            while socket.next().await.is_some() {}
        });
        let connection =
            Arc::new(Connection::start(Target::socket(path, false, None).unwrap()).unwrap());
        let event = time::timeout(Duration::from_secs(5), connection.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, Event::Ready(info) if info.server_id == "socket-fixture"));
        connection.close();
        server.abort();
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[cfg(unix)]
    #[test]
    fn a_named_pipe_target_fails_honestly_on_unix() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let target = Target::socket(PathBuf::from(r"\\.\pipe\paseo"), true, None).unwrap();
        let error = match runtime.block_on(dial(&target)) {
            Ok(_) => panic!("a named pipe cannot dial on unix"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("Windows named pipe"), "{error}");
    }

    #[test]
    fn release_commands_reserve_nothing_and_reservations_release_exactly_once() {
        let ledger = AtomicU64::new(0);
        // A queued send reserves its payload bytes.
        let send = Command::Send(1, Frame::Text("abc".into()));
        assert_eq!(send.reserved_bytes(), 3);
        let reservation = QueueReservation {
            ledger: &ledger,
            bytes: send.reserved_bytes(),
        };
        ledger.fetch_add(3, Ordering::SeqCst);
        // A send that fails after dequeue releases through Drop, exactly once.
        drop(reservation);
        assert_eq!(ledger.load(Ordering::SeqCst), 0);
        // A release command reserves nothing, so consuming or draining one can
        // never underflow the ledger.
        let release = Command::Release(1, "subscription-id".into());
        assert_eq!(release.reserved_bytes(), 0);
        let reservation = QueueReservation {
            ledger: &ledger,
            bytes: release.reserved_bytes(),
        };
        drop(reservation);
        assert_eq!(ledger.load(Ordering::SeqCst), 0);
        // Successful delivery returns its reservation before the next enqueue.
        let reservation = QueueReservation {
            ledger: &ledger,
            bytes: 5,
        };
        ledger.fetch_add(5, Ordering::SeqCst);
        reservation.release();
        assert_eq!(ledger.load(Ordering::SeqCst), 0);
        // Repeated successful sends can exceed the lifetime queue budget while
        // the amount currently queued remains bounded.
        for _ in 0..128 {
            let bytes = 1024 * 1024;
            ledger.fetch_add(bytes, Ordering::SeqCst);
            QueueReservation {
                ledger: &ledger,
                bytes,
            }
            .release();
            assert_eq!(ledger.load(Ordering::SeqCst), 0);
        }
    }

    #[test]
    fn image_and_escaped_text_serialization_never_grows_beyond_the_wire_budget() {
        let value = json!({"images":[{"data":"x".repeat(200),"mimeType":"image/png"}],"text":"\u{1}".repeat(30)});
        let mut bounded = BoundedJson::new(128);
        assert!(serde_json::to_writer(&mut bounded, &value).is_err());
        assert!(bounded.bytes.len() <= 128);
        assert!(bounded.bytes.capacity() <= 128);
        let mut bounded = BoundedJson::new(1024);
        serde_json::to_writer(&mut bounded, &value).unwrap();
        assert_eq!(bounded.bytes, serde_json::to_vec(&value).unwrap());
    }

    #[tokio::test]
    async fn reconnect_handshake_reuses_host_but_never_replays_mutations_or_old_subscriptions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for attempt in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                let hello = socket.next().await.unwrap().unwrap();
                assert!(hello.to_text().unwrap().contains("hello"));
                socket.send(Message::Text(json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":"same-host","features":{}}}}).to_string())).await.unwrap();
                let message = socket.next().await.unwrap().unwrap();
                let text = message.to_text().unwrap();
                if attempt == 0 {
                    assert!(text.contains("mutate-once"));
                    socket.close(None).await.unwrap();
                } else {
                    assert!(text.contains("fresh-read"));
                    assert!(!text.contains("mutate-once"));
                }
            }
        });
        let connection =
            Connection::start(Target::remote(format!("ws://{address}/ws"), None).unwrap()).unwrap();
        assert!(matches!(
            time::timeout(Duration::from_secs(3), connection.events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Ready(_)
        ));
        assert!(connection.connected());
        connection.request("mutate-once", json!({})).unwrap();
        assert!(matches!(
            time::timeout(Duration::from_secs(3), connection.events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Disconnected(_)
        ));
        assert!(!connection.connected());
        assert!(connection.request("offline-mutation", json!({})).is_err());
        assert!(matches!(
            time::timeout(Duration::from_secs(3), connection.events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Ready(_)
        ));
        assert!(connection.connected());
        connection.request("fresh-read", json!({})).unwrap();
        server.await.unwrap();
        connection.close();
    }

    #[tokio::test]
    async fn a_different_host_on_reconnect_is_fatal_and_never_becomes_ready() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for (attempt, server_id) in ["pinned-host", "other-host"].iter().enumerate() {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                socket.next().await.unwrap().unwrap();
                socket.send(Message::Text(json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":server_id,"features":{}}}}).to_string())).await.unwrap();
                if attempt == 0 {
                    socket.close(None).await.unwrap();
                }
                while socket.next().await.is_some() {}
            }
        });
        let connection =
            Connection::start(Target::remote(format!("ws://{address}/ws"), None).unwrap()).unwrap();
        assert!(matches!(
            time::timeout(Duration::from_secs(3), connection.events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Ready(_)
        ));
        let error = match time::timeout(Duration::from_secs(3), async {
            loop {
                match connection.events.recv().await {
                    Ok(Event::Disconnected(error)) if error.contains("different host") => {
                        break error;
                    }
                    // The first drop is the socket the fixture closed.
                    Ok(Event::Disconnected(_)) => continue,
                    Ok(_) => continue,
                    Err(error) => panic!("connection events closed: {error}"),
                }
            }
        })
        .await
        {
            Ok(error) => error,
            Err(_) => panic!("a re-pinned host must report a disconnect"),
        };
        assert!(error.contains("different host"), "{error}");
        // Fatal: the supervisor stops instead of reconnecting into a
        // neighbour's daemon. A stopped supervisor drops the event sender, so
        // either nothing arrives or the channel closes.
        match time::timeout(Duration::from_millis(750), connection.events.recv()).await {
            Ok(Ok(_)) => panic!("a fatal identity change must not keep reconnecting"),
            Ok(Err(_)) | Err(_) => {}
        }
        assert!(!connection.connected());
        server.abort();
        connection.close();
    }

    #[tokio::test]
    async fn closing_client_cancels_bulk_transfer_when_inbound_delivery_is_backpressured() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            socket.next().await.unwrap().unwrap();
            socket.send(Message::Text(json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":"fixture","features":{}}}}).to_string())).await.unwrap();
            for _ in 0..12 {
                if socket
                    .send(Message::Text(
                        json!({"type":"session","message":{"type":"fixture.event","payload":{}}})
                            .to_string(),
                    ))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            while socket.next().await.is_some() {}
        });
        let connection = Arc::new(
            Connection::start(Target::remote(format!("ws://{address}/ws"), None).unwrap()).unwrap(),
        );
        assert!(matches!(
            connection.events.recv().await.unwrap(),
            Event::Ready(_)
        ));
        let path =
            std::env::temp_dir().join(format!("paseo-native-upload-close-{}", Uuid::new_v4()));
        let mut file = fs::File::create(&path).unwrap();
        std::io::Write::write_all(&mut file, &vec![7; 2 * 1024 * 1024]).unwrap();
        drop(file);
        let crate::attachments::Prepared::File(file) =
            crate::attachments::prepare(&path, false).unwrap()
        else {
            panic!("expected file")
        };
        let sender = connection.clone();
        let upload = tokio::spawn(async move {
            sender
                .upload_file(file, "fixture-upload", Arc::new(AtomicBool::new(false)))
                .await
        });
        // This waits for a verifiably full event queue, not a wall-clock guess about delivery.
        time::timeout(Duration::from_secs(2), async {
            while connection.events.len() < 4 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        connection.close();
        assert!(
            time::timeout(Duration::from_secs(2), upload)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        server.abort();
        fs::remove_file(path).unwrap();
    }
}

#[cfg(test)]
mod relay_tests {
    use super::*;
    use crate::relay::{NOISE_PROLOGUE, NOISE_PROTOCOL_NAME, NegotiatedParams, ProtectedChannel};
    use snow::Builder;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn daemon_keypair() -> snow::Keypair {
        Builder::new(NOISE_PROTOCOL_NAME.parse().unwrap())
            .generate_keypair()
            .unwrap()
    }

    fn relay_target(address: std::net::SocketAddr, key: &snow::Keypair) -> Target {
        Target::relay(
            format!("ws://{address}/ws"),
            b64(&key.public),
            Some("secret-password".into()),
        )
        .unwrap()
    }

    /// Accepts one relay connection and completes the protected responder
    /// side, asserting no credential ever travels before the ready. Returns
    /// the daemon's record channel and the decrypted daemon hello text.
    async fn accept_handshake(
        listener: &tokio::net::TcpListener,
        daemon_static: &snow::Keypair,
    ) -> (
        ProtectedChannel,
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        String,
        Vec<u8>,
    ) {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let first = socket.next().await.unwrap().unwrap();
        let text = first.to_text().unwrap().to_string();
        let hello: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(hello["type"], "e2ee_hello", "{text}");
        assert!(
            hello["protected"].is_object(),
            "upgrade must offer protected"
        );
        assert!(
            hello["clientId"].is_null() && hello.get("auth").is_none(),
            "credentials must never ride the handshake: {text}"
        );
        let params: snow::params::NoiseParams = NOISE_PROTOCOL_NAME.parse().unwrap();
        let mut daemon = Builder::new(params)
            .local_private_key(&daemon_static.private)
            .expect("set the daemon static key")
            .prologue(NOISE_PROLOGUE)
            .expect("set the prologue")
            .build_responder()
            .unwrap();
        let msg1 = base64::engine::general_purpose::STANDARD
            .decode(hello["protected"]["message"].as_str().unwrap())
            .unwrap();
        let mut payload = vec![0u8; 65535];
        let written = daemon.read_message(&msg1, &mut payload).unwrap();
        assert_eq!(&payload[..written], &[0x01, 0x01]);
        let written = daemon.write_message(&[0x01, 0x01], &mut payload).unwrap();
        socket
            .send(Message::Text(
                json!({"type":"e2ee_ready","protected":{"v":1,"message":b64(&payload[..written])}})
                    .to_string(),
            ))
            .await
            .unwrap();
        let mut channel = ProtectedChannel::from_transport(
            daemon.into_transport_mode().unwrap(),
            NegotiatedParams {
                binary_ciphertext: true,
            },
        );
        // The daemon hello (with credentials) arrives only after the ready.
        let next = socket.next().await.unwrap().unwrap();
        let record = base64::engine::general_purpose::STANDARD
            .decode(next.to_text().unwrap())
            .unwrap();
        let frame = channel.decode_record(&record).unwrap();
        assert_eq!(frame.kind, crate::relay::FrameKind::Text);
        let daemon_hello = String::from_utf8(frame.data).unwrap();
        let hello: Value = serde_json::from_str(&daemon_hello).unwrap();
        assert_eq!(hello["type"], "hello");
        assert_eq!(
            hello["auth"]["kind"],
            "localCredential".replace("localCredential", "password")
        );
        assert_eq!(hello["auth"]["password"], "secret-password");
        (channel, socket, daemon_hello, msg1)
    }

    type RawSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    async fn send_server_info(
        channel: &mut ProtectedChannel,
        socket: &mut RawSocket,
        server_id: &str,
    ) {
        let info = json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":server_id,"features":{}}}})
            .to_string();
        let record = channel
            .encode_frame(crate::relay::FrameKind::Text, info.as_bytes())
            .unwrap();
        socket.send(Message::Text(b64(&record))).await.unwrap();
    }

    #[tokio::test]
    async fn an_oversized_relay_send_fails_at_accept_without_dropping_the_channel() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let key = daemon_keypair();
        let server_static = snow::Keypair {
            private: key.private.clone(),
            public: key.public.clone(),
        };
        let server = tokio::spawn(async move {
            let (mut channel, mut socket, _daemon_hello, _msg1) =
                accept_handshake(&listener, &server_static).await;
            // server_info without binaryChunkingV1: no outer chunking.
            send_server_info(&mut channel, &mut socket, "relay-host").await;
            // The oversized request never arrives. The next frame is the small
            // request the client sends after its rejected enqueue.
            let next = socket.next().await.unwrap().unwrap();
            let record = base64::engine::general_purpose::STANDARD
                .decode(next.to_text().unwrap())
                .unwrap();
            let frame = channel.decode_record(&record).unwrap();
            let text = String::from_utf8(frame.data).unwrap();
            assert!(text.contains("small-after-rejection"), "{text}");
            while socket.next().await.is_some() {}
        });
        let connection = Connection::start(relay_target(address, &key)).unwrap();
        let Event::Ready(_) = time::timeout(Duration::from_secs(5), connection.events.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected ready");
        };
        let oversized = "x".repeat(26 * 1024 * 1024);
        let error = connection
            .send_session_data(&json!({"text": oversized}))
            .unwrap_err();
        // The request fails at accept, with the actionable reason, and the
        // protected channel stays up.
        assert!(error.to_string().contains("binaryChunkingV1"), "{error}");
        assert!(
            error.to_string().contains("Update the host daemon"),
            "{error}"
        );
        connection
            .request("small-after-rejection", json!({}))
            .unwrap();
        server.abort();
        connection.close();
    }

    #[tokio::test]
    async fn relay_target_negotiates_protected_security_before_any_credential_leaves() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let key = daemon_keypair();
        let server_static = snow::Keypair {
            private: key.private.clone(),
            public: key.public.clone(),
        };
        let server = tokio::spawn(async move {
            let (mut channel, mut socket, _daemon_hello, _msg1) =
                accept_handshake(&listener, &server_static).await;
            send_server_info(&mut channel, &mut socket, "relay-host").await;
            // The client's next application frame arrives encrypted.
            let next = socket.next().await.unwrap().unwrap();
            let record = base64::engine::general_purpose::STANDARD
                .decode(next.to_text().unwrap())
                .unwrap();
            let frame = channel.decode_record(&record).unwrap();
            let text = String::from_utf8(frame.data).unwrap();
            assert!(text.contains("relay-session-request"), "{text}");
            while socket.next().await.is_some() {}
        });
        let connection = Connection::start(relay_target(address, &key)).unwrap();
        let Event::Ready(info) = time::timeout(Duration::from_secs(5), connection.events.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected ready");
        };
        assert_eq!(info.server_id, "relay-host");
        assert_eq!(connection.negotiated_security(), Some("protected"));
        connection
            .request("relay-session-request", json!({}))
            .unwrap();
        server.abort();
        connection.close();
    }

    #[tokio::test]
    async fn legacy_ready_is_a_fatal_4002_that_stops_reconnecting() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let key = daemon_keypair();
        let _server_static = snow::Keypair {
            private: key.private.clone(),
            public: key.public.clone(),
        };
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let first = socket.next().await.unwrap().unwrap();
            assert!(first.to_text().unwrap().contains("e2ee_hello"));
            socket
                .send(Message::Text(
                    json!({"type":"e2ee_ready","capabilities":{"binaryCiphertext":true}})
                        .to_string(),
                ))
                .await
                .unwrap();
            // The close code must actually reach the relay on the wire, not
            // only appear in the client's local error.
            let close = time::timeout(Duration::from_secs(3), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            match close {
                Message::Close(Some(frame)) => {
                    assert_eq!(u16::from(frame.code), 4002, "{frame:?}");
                }
                other => panic!("expected a 4002 close frame, got {other:?}"),
            }
            // No second connection may ever arrive: 4002 is fatal.
            let second = time::timeout(Duration::from_millis(1500), listener.accept()).await;
            assert!(second.is_err(), "client must not reconnect after a 4002");
        });
        let connection = Connection::start(relay_target(address, &key)).unwrap();
        let Event::Disconnected(error) =
            time::timeout(Duration::from_secs(5), connection.events.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected disconnect");
        };
        assert!(error.contains("4002"), "{error}");
        assert!(error.contains("legacy endpoint security"), "{error}");
        // The 4002 message tells the user what actually fixes the failure.
        assert!(
            error.contains("Update the host daemon and relay"),
            "{error}"
        );
        server.await.unwrap();
        connection.close();
    }

    #[tokio::test]
    async fn a_failed_record_is_4003_and_reconnects_with_a_fresh_handshake() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let key = daemon_keypair();
        let server_static = snow::Keypair {
            private: key.private.clone(),
            public: key.public.clone(),
        };
        let server = tokio::spawn(async move {
            let mut first_msg1: Option<Vec<u8>> = None;
            let mut first_client_id: Option<String> = None;
            for attempt in 0..2 {
                let (mut channel, mut socket, daemon_hello, msg1) =
                    accept_handshake(&listener, &server_static).await;
                // A 4003 reconnect must run a genuinely fresh handshake: new
                // ephemeral in message 1, new clientId in the daemon hello.
                let hello: Value = serde_json::from_str(&daemon_hello).unwrap();
                let client_id = hello["clientId"].as_str().unwrap().to_string();
                if let Some(previous) = &first_msg1 {
                    assert_ne!(previous, &msg1, "reconnect must use a fresh ephemeral");
                } else {
                    first_msg1 = Some(msg1);
                }
                if let Some(previous) = &first_client_id {
                    assert_ne!(previous, &client_id, "reconnect must use a fresh clientId");
                } else {
                    first_client_id = Some(client_id);
                }
                send_server_info(&mut channel, &mut socket, "relay-host").await;
                if attempt == 0 {
                    // Corrupt one byte of a valid record: authentication fails.
                    let info = json!({"type":"session","message":{"type":"status","payload":{"status":"server_info","serverId":"relay-host","features":{}}}}).to_string();
                    let mut record = channel
                        .encode_frame(crate::relay::FrameKind::Text, info.as_bytes())
                        .unwrap();
                    let last = record.len() - 1;
                    record[last] ^= 0x01;
                    socket.send(Message::Text(b64(&record))).await.unwrap();
                    // The client must report the failure to the relay as a
                    // real 4003 close before dropping the socket.
                    let close = time::timeout(Duration::from_secs(3), socket.next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap();
                    match close {
                        Message::Close(Some(frame)) => {
                            assert_eq!(u16::from(frame.code), 4003, "{frame:?}");
                        }
                        other => panic!("expected a 4003 close frame, got {other:?}"),
                    }
                } else {
                    while socket.next().await.is_some() {}
                }
            }
        });
        let connection = Connection::start(relay_target(address, &key)).unwrap();
        let Event::Ready(_) = time::timeout(Duration::from_secs(5), connection.events.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected first ready");
        };
        let Event::Disconnected(error) =
            time::timeout(Duration::from_secs(5), connection.events.recv())
                .await
                .unwrap()
                .unwrap()
        else {
            panic!("expected record failure");
        };
        assert!(error.contains("4003"), "{error}");
        let Event::Ready(info) = time::timeout(Duration::from_secs(5), connection.events.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected reconnect ready");
        };
        assert_eq!(info.server_id, "relay-host");
        server.abort();
        connection.close();
    }

    #[tokio::test]
    async fn hello_retries_resend_the_exact_cached_text_until_the_ready() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let key = daemon_keypair();
        let server_static = snow::Keypair {
            private: key.private.clone(),
            public: key.public.clone(),
        };
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            let first = socket.next().await.unwrap().unwrap();
            let first_text = first.to_text().unwrap().to_string();
            // The client resends the identical hello every second.
            let second = time::timeout(Duration::from_secs(4), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                second.to_text().unwrap(),
                first_text,
                "retries must be byte-identical"
            );
            let hello: Value = serde_json::from_str(&first_text).unwrap();
            let params: snow::params::NoiseParams = NOISE_PROTOCOL_NAME.parse().unwrap();
            let mut daemon = Builder::new(params)
                .local_private_key(&server_static.private)
                .expect("set the daemon static key")
                .prologue(NOISE_PROLOGUE)
                .expect("set the prologue")
                .build_responder()
                .unwrap();
            let msg1 = base64::engine::general_purpose::STANDARD
                .decode(hello["protected"]["message"].as_str().unwrap())
                .unwrap();
            let mut payload = vec![0u8; 65535];
            daemon.read_message(&msg1, &mut payload).unwrap();
            let written = daemon.write_message(&[0x01, 0x01], &mut payload).unwrap();
            socket
                .send(Message::Text(
                    json!({"type":"e2ee_ready","protected":{"v":1,"message":b64(&payload[..written])}})
                        .to_string(),
                ))
                .await
                .unwrap();
            // Complete the vertical: decode the daemon hello so the retry
            // proof ends in an open, authenticated channel the client uses.
            let mut channel = ProtectedChannel::from_transport(
                daemon.into_transport_mode().unwrap(),
                NegotiatedParams {
                    binary_ciphertext: true,
                },
            );
            let next = socket.next().await.unwrap().unwrap();
            let record = base64::engine::general_purpose::STANDARD
                .decode(next.to_text().unwrap())
                .unwrap();
            let frame = channel.decode_record(&record).unwrap();
            let daemon_hello: Value =
                serde_json::from_str(&String::from_utf8(frame.data).unwrap()).unwrap();
            assert_eq!(daemon_hello["type"], "hello");
            send_server_info(&mut channel, &mut socket, "relay-host").await;
            while socket.next().await.is_some() {}
        });
        let connection = Connection::start(relay_target(address, &key)).unwrap();
        // Await the ready the retried hello actually produced.
        let Event::Ready(info) = time::timeout(Duration::from_secs(8), connection.events.recv())
            .await
            .unwrap()
            .unwrap()
        else {
            panic!("expected ready after the retried hello");
        };
        assert_eq!(info.server_id, "relay-host");
        connection.close();
        server.await.unwrap();
    }
}
