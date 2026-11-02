use anyhow::{Context, Result, ensure};
use paseo_desktop::{
    client::{Connection, Event, Target},
    terminal::{GridSize, TerminalBuffer},
    wire::TerminalOpcode,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};
fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("fixture.json path required")?;
    let meta: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let connection = Connection::start(Target::remote(
        format!(
            "ws://127.0.0.1:{}/ws",
            meta["port"].as_u64().context("port")?
        ),
        None,
    )?)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut buffer = TerminalBuffer::new(GridSize { rows: 24, cols: 80 }, 5000);
    let mut request = None;
    let mut slot = None;
    let mut chunks = Vec::new();
    let mut complete = false;
    while Instant::now() < deadline && !complete {
        let event = match connection.events.try_recv() {
            Ok(event) => event,
            Err(async_channel::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            Err(error) => anyhow::bail!("{error}"),
        };
        match event {
            Event::Ready(_) => {
                request=Some(connection.subscribe("utf8-replay-qa","subscribe_terminal_request",json!({"terminalId":meta["terminalId"],"restore":{"mode":"full-snapshot","scrollbackLines":5000,"size":{"rows":24,"cols":80}}}))?);
            }
            Event::Session { kind, payload }
                if kind == "subscribe_terminal_response"
                    && payload["requestId"].as_str() == request.as_deref() =>
            {
                ensure!(
                    payload.get("error").is_none_or(Value::is_null),
                    "subscription: {payload}"
                );
                slot = payload["slot"]
                    .as_u64()
                    .and_then(|value| u8::try_from(value).ok());
            }
            Event::Terminal(frame) => {
                ensure!(slot == Some(frame.slot), "frame has no owned slot");
                if frame.opcode == TerminalOpcode::ReplayData {
                    chunks.push(frame.data.len());
                }
                complete = frame.opcode == TerminalOpcode::ReplayEnd;
                buffer.apply(frame)?;
            }
            Event::Disconnected(error) => anyhow::bail!("{error}"),
            _ => {}
        }
    }
    ensure!(complete, "Replay did not complete before deadline");
    ensure!(
        chunks.len() > 1,
        "Fixture did not exercise incremental replay"
    );
    let copied = paseo_desktop::terminal_copy::text(
        &buffer,
        -(buffer.history_size() as i32),
        0,
        i32::from(buffer.size.rows) - 1,
        usize::from(buffer.size.cols) - 1,
        false,
        false,
    );
    let actual: Vec<_> = copied
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let mut expected: Vec<_> = (0..meta["count"].as_u64().context("count")?)
        .map(|index| format!("{index:05} e\u{301} 中 box─ end"))
        .collect();
    expected.push("WIRE-END".into());
    ensure!(
        actual == expected,
        "Replay content differs: {} actual rows vs {} expected",
        actual.len(),
        expected.len()
    );
    let result = json!({"result":"passed","rows":actual.len(),"replayChunks":chunks,"historyRows":buffer.history_size(),"first":actual.first(),"last":actual.last()});
    fs::write(
        Path::new(&path)
            .parent()
            .context("fixture parent")?
            .join("reader-result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}
