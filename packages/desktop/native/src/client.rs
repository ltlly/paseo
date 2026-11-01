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
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{
    net::TcpStream,
    sync::{mpsc, watch},
    time,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use uuid::Uuid;

use crate::{
    model::ServerInfo,
    wire::{ChunkDecoder, Frame, MAX_MESSAGE_BYTES, TerminalFrame},
};

#[derive(Clone)]
pub struct Target {
    pub url: String,
    auth: Option<Value>,
    home: Option<PathBuf>,
}

impl Target {
    pub fn local(home: &Path) -> Result<Self> {
        let state: Value = serde_json::from_slice(
            &fs::read(home.join("paseo.pid")).context("read local daemon discovery")?,
        )?;
        let listen = state["listen"]
            .as_str()
            .context("local daemon has no TCP listener")?;
        ensure!(
            listen.starts_with("127.0.0.1:")
                || listen.starts_with("localhost:")
                || listen.starts_with("[::1]:"),
            "local TCP discovery must use loopback"
        );
        let token = match fs::read_to_string(home.join("local-credential")) {
            Ok(token) => Some(json!({"kind":"localCredential","token":token.trim()})),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("read local daemon credential"),
        };
        Ok(Self {
            url: format!("ws://{listen}/ws"),
            auth: token,
            home: Some(home.into()),
        })
    }

    pub fn remote(url: String, password: Option<String>) -> Result<Self> {
        ensure!(
            url.starts_with("ws://") || url.starts_with("wss://"),
            "endpoint must use ws:// or wss://"
        );
        Ok(Self {
            url,
            auth: password.map(|password| json!({"kind":"password","password":password})),
            home: None,
        })
    }
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
    Disconnected(String),
}

enum Command {
    Send(u64, Frame),
    Release(u64, String),
}

pub struct Connection {
    subscriptions: Arc<Mutex<crate::subscriptions::Subscriptions>>,
    commands: mpsc::Sender<Command>,
    uploads: mpsc::Sender<(u64, Frame)>,
    ready: Arc<AtomicU64>,
    upload_state: watch::Sender<u64>,
    shutdown: watch::Sender<bool>,
    pub events: Receiver<Event>,
}

impl Connection {
    pub fn start(target: Target) -> Result<Self> {
        let subscriptions = Arc::new(Mutex::new(crate::subscriptions::Subscriptions::default()));
        let network_subscriptions = subscriptions.clone();
        let (commands, receiver) = mpsc::channel(256);
        // Bulk transfers have two bounded frames and yield to interactive input and replies.
        let (uploads, upload_receiver) = mpsc::channel(2);
        let (shutdown, mut shutdown_receiver) = watch::channel(false);
        let ready = Arc::new(AtomicU64::new(0));
        let generation = Arc::new(AtomicU64::new(0));
        let network_ready = ready.clone();
        let network_generation = generation.clone();
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
                        runtime.block_on(async {tokio::select!{result=supervise(target, receiver, upload_receiver, &events, network_subscriptions, network_ready, network_generation, network_upload_state)=>result,_=shutdown_receiver.changed()=>Ok(())}})
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
            events: event_receiver,
        })
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
        let generation = self.ready.load(Ordering::SeqCst);
        ensure!(
            generation != 0,
            "Host is disconnected; input was not queued"
        );
        self.commands
            .try_send(Command::Send(generation, frame))
            .context("connection input queue is unavailable")
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

async fn send(socket: &mut Socket, frame: Frame, chunked: bool) -> Result<()> {
    const FRAME_BYTES: usize = 1024 * 1024;
    let (kind, bytes) = match frame {
        Frame::Text(text) if !chunked || text.len() <= FRAME_BYTES => {
            socket.send(Message::Text(text)).await?;
            return Ok(());
        }
        Frame::Binary(bytes) if !chunked || bytes.len() <= FRAME_BYTES => {
            socket.send(Message::Binary(bytes)).await?;
            return Ok(());
        }
        Frame::Text(text) => (0, text.into_bytes()),
        Frame::Binary(bytes) => (1, bytes),
    };
    let count = bytes.len().div_ceil(FRAME_BYTES - 4);
    for (index, chunk) in bytes.chunks(FRAME_BYTES - 4).enumerate() {
        let flags = u8::from(index == 0) | (u8::from(index + 1 == count) << 1);
        let mut frame = Vec::with_capacity(chunk.len() + 4);
        frame.extend_from_slice(&[0x31, 1, kind, flags]);
        frame.extend_from_slice(chunk);
        socket.send(Message::Binary(frame)).await?;
    }
    Ok(())
}

async fn release_subscription(
    socket: &mut WebSocketStream<MaybeTlsStream<TcpStream>>,
    id: String,
    chunked: bool,
) -> Result<()> {
    send(socket, Frame::Text(json!({"type":"session","message":{"type":"subscription.release.request","requestId":Uuid::new_v4().to_string(),"subscriptionId":id}}).to_string()), chunked).await
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
                )
                .await
            }
            Err(error) => Err(error),
        };
        ready.store(0, Ordering::SeqCst);
        upload_state.send_replace(0);
        subscriptions.lock().expect("subscription lock").reset();
        while commands.try_recv().is_ok() {}
        while uploads.try_recv().is_ok() {}
        let Err(error) = result else {
            return Ok(());
        };
        let fatal = error.to_string().contains("rejected authentication")
            || error.to_string().contains("different host")
            || error.to_string().contains("invalid daemon envelope")
            || error.to_string().contains("invalid server info")
            || error.to_string().contains("session message has no type");
        events
            .send(Event::Disconnected(format!("{error:#}")))
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
) -> Result<()> {
    let config = WebSocketConfig {
        max_message_size: Some(MAX_MESSAGE_BYTES),
        max_frame_size: Some(MAX_MESSAGE_BYTES),
        ..Default::default()
    };
    let (mut socket, _) = time::timeout(
        Duration::from_secs(15),
        connect_async_with_config(&target.url, Some(config), false),
    )
    .await
    .context("daemon connection timed out")??;
    let mut hello = json!({"type":"hello","clientId":Uuid::new_v4().to_string(),"clientType":"browser","protocolVersion":1,"appVersion":env!("CARGO_PKG_VERSION"),"capabilities":{
        "hello_rejection":true,"explicit_event_subscriptions":true,"all_providers":true,"selective_agent_timeline":true,"terminal_chunked_replay":true,"binary_chunking_v1":true,"project_updates":true,"timeline_replacement_invalidation":true,"owned_subscriptions":true
    }});
    if let Some(auth) = target.auth {
        hello["auth"] = auth;
    }
    socket.send(Message::Text(hello.to_string())).await?;
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
            match command {
                Command::Send(epoch, frame) if ready && epoch == generation => {
                    send(&mut socket, frame, chunked).await?
                }
                Command::Release(epoch, id) if ready && epoch == generation => {
                    release_subscription(&mut socket, id, chunked).await?
                }
                _ => {}
            }
            continue;
        }
        let release = subscriptions
            .lock()
            .expect("subscription lock")
            .next_release();
        if let Some(id) = release {
            release_subscription(&mut socket, id, chunked).await?;
            continue;
        }
        // Give bulk data a turn between inbound records, even while a terminal streams.
        if ready
            && bulk_turn
            && let Ok((epoch, frame)) = uploads.try_recv()
        {
            bulk_turn = false;
            if epoch == generation {
                send(&mut socket, frame, chunked).await?;
            }
            continue;
        }
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(Command::Send(epoch, frame)) if ready && epoch == generation => send(&mut socket, frame, chunked).await?,
                Some(Command::Release(epoch, id)) if ready && epoch == generation => release_subscription(&mut socket, id, chunked).await?,
                Some(_) => {},
                None => { socket.close(None).await?; return Ok(()); }
            },
            _ = time::sleep_until(handshake_deadline), if !ready => bail!("daemon handshake timed out"),
            _ = heartbeat.tick() => {
                ensure!(last_received.elapsed() < Duration::from_secs(45), "daemon heartbeat timed out");
                socket.send(Message::Text(json!({"type":"ping"}).to_string())).await?;
            }
            message = socket.next() => {
                last_received = time::Instant::now();
                bulk_turn=true;
                let frame = match message.context("daemon disconnected")?? {
                    Message::Text(text) => Frame::Text(text),
                    Message::Binary(bytes) => Frame::Binary(bytes),
                    Message::Ping(bytes) => { socket.send(Message::Pong(bytes)).await?; continue; }
                    Message::Pong(_) => continue,
                    Message::Close(_) => bail!("daemon closed connection"),
                    Message::Frame(_) => continue,
                };
                let Some(frame) = chunks.receive(frame)? else { continue; };
                match frame {
                    Frame::Binary(bytes) => {
                        if bytes.first().is_some_and(|op| (1..=8).contains(op)) {
                            events.send(Event::Terminal(TerminalFrame::decode(bytes)?)).await?;
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
                if epoch == generation { send(&mut socket,frame,chunked).await?; }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        connection.request("mutate-once", json!({})).unwrap();
        assert!(matches!(
            time::timeout(Duration::from_secs(3), connection.events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Disconnected(_)
        ));
        assert!(connection.request("offline-mutation", json!({})).is_err());
        assert!(matches!(
            time::timeout(Duration::from_secs(3), connection.events.recv())
                .await
                .unwrap()
                .unwrap(),
            Event::Ready(_)
        ));
        connection.request("fresh-read", json!({})).unwrap();
        server.await.unwrap();
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
