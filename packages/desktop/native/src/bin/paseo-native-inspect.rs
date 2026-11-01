use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use paseo_desktop::{
    client::{Connection, Event, Target, default_home},
    model::{Directory, WorkspacePage},
};
use serde_json::json;

fn main() -> Result<()> {
    let target = match std::env::args().nth(1) {
        Some(url) => Target::remote(url, std::env::var("PASEO_PASSWORD").ok())?,
        None => Target::local(&default_home()?)?,
    };
    let connection = Connection::start(target)?;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut directory = Directory::default();
    let mut requests = std::collections::HashSet::new();
    let info = loop {
        let event = receive(&connection, deadline)?;
        match event {
            Event::Ready(info) => break info,
            Event::Disconnected(error) => anyhow::bail!("{error}"),
            _ => {}
        }
    };
    for kind in [
        "project.list.request",
        "fetch_workspaces_request",
        "list_terminals_request",
    ] {
        requests.insert(connection.request(kind, json!({}))?);
    }
    while !requests.is_empty() {
        match receive(&connection, deadline)? {
            Event::Session { kind, payload } => {
                if kind == "rpc_error" {
                    anyhow::bail!("daemon RPC failed: {}", payload["error"]);
                }
                if let Some(id) = payload["requestId"].as_str() {
                    requests.remove(id);
                }
                if kind == "fetch_workspaces_response" {
                    let page: WorkspacePage = serde_json::from_value(payload.clone())?;
                    if page.page_info.has_more {
                        let cursor = page
                            .page_info
                            .next_cursor
                            .context("workspace page has no continuation cursor")?;
                        requests.insert(connection.request(
                            "fetch_workspaces_request",
                            json!({"page":{"cursor":cursor}}),
                        )?);
                    }
                }
                directory.apply(&kind, payload)?;
            }
            Event::Disconnected(error) => anyhow::bail!("{error}"),
            _ => {}
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"serverId":info.server_id,"hostname":info.hostname,"version":info.version,"projects":directory.projects.len(),"workspaces":directory.workspaces.len(),"terminals":directory.terminals.len()})
        )?
    );
    Ok(())
}

fn receive(connection: &Connection, deadline: Instant) -> Result<Event> {
    while Instant::now() < deadline {
        match connection.events.try_recv() {
            Ok(event) => return Ok(event),
            Err(async_channel::TryRecvError::Closed) => anyhow::bail!("connection stopped"),
            Err(async_channel::TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    anyhow::bail!("native daemon inspection timed out")
}
