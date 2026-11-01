use std::{
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use async_channel::{Receiver, Sender};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpStream, sync::mpsc, time};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use uuid::Uuid;

use crate::{
    model::ServerInfo,
    wire::{ChunkDecoder, Frame, MAX_MESSAGE_BYTES, TerminalFrame},
};

pub struct Target {
    pub url: String,
    auth: Option<Value>,
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
    Send(Frame),
}

pub struct Connection {
    commands: mpsc::Sender<Command>,
    pub events: Receiver<Event>,
}

impl Connection {
    pub fn start(target: Target) -> Result<Self> {
        let (commands, receiver) = mpsc::channel(256);
        let (events, event_receiver) = async_channel::bounded(4);
        std::thread::Builder::new()
            .name("paseo-connection".into())
            .spawn(move || {
                let result = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("start network runtime")
                    .and_then(|runtime| runtime.block_on(run(target, receiver, &events)));
                if let Err(error) = result {
                    let _ = events.send_blocking(Event::Disconnected(format!("{error:#}")));
                }
            })?;
        Ok(Self {
            commands,
            events: event_receiver,
        })
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

    pub fn send_session(&self, message: Value) -> Result<()> {
        self.send(Frame::Text(serde_json::to_string(
            &json!({"type":"session","message":message}),
        )?))
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
        self.commands
            .try_send(Command::Send(frame))
            .context("connection input queue is unavailable")
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

async fn run(
    target: Target,
    mut commands: mpsc::Receiver<Command>,
    events: &Sender<Event>,
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
        "hello_rejection":true,"explicit_event_subscriptions":true,"all_providers":true,"selective_agent_timeline":true,"terminal_chunked_replay":true,"binary_chunking_v1":true,"project_updates":true
    }});
    if let Some(auth) = target.auth {
        hello["auth"] = auth;
    }
    socket.send(Message::Text(hello.to_string())).await?;
    let mut chunks = ChunkDecoder::default();
    let mut chunked = false;
    let mut ready = false;
    let mut heartbeat = time::interval(Duration::from_secs(15));
    heartbeat.tick().await;
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Send(frame)) => send(&mut socket, frame, chunked).await?,
                None => { socket.close(None).await?; return Ok(()); }
            },
            _ = heartbeat.tick() => socket.send(Message::Text(json!({"type":"ping"}).to_string())).await?,
            message = socket.next() => {
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
                            ready = true;
                            events.send(Event::Ready(info)).await?;
                        } else if ready {
                            events.send(Event::Session { kind: kind.into(), payload }).await?;
                        }
                    }
                }
            }
        }
    }
}
