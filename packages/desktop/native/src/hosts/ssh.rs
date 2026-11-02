//! SSH transport: a fresh `ssh -W` process per forwarded connection reaches
//! an already running daemon, mirroring the shared Node connector
//! (`ssh-tunnel.ts`). The local listener lives as long as the tunnel, so a
//! supervising client reconnects through the same endpoint: each accepted
//! connection spawns a new ssh child.
//! Process ownership: a forwarded connection owns its child, reaps it when
//! the connection ends, and never signals a pid it no longer owns.

use std::process::Stdio;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};

use super::target::{SshTransportTarget, build_ssh_tunnel_args};

const SSH_STDERR_LIMIT: usize = 8192;
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone, Default)]
struct FailureSlot(Arc<Mutex<Option<String>>>);

impl FailureSlot {
    fn set_if_absent(&self, detail: String) {
        let mut guard = self.0.lock().expect("ssh failure lock");
        if detail.trim().is_empty() {
            return;
        }
        if guard.is_none() {
            *guard = Some(detail);
        }
    }

    pub fn detail(&self) -> Option<String> {
        self.0
            .lock()
            .expect("ssh failure lock")
            .clone()
            .filter(|detail| !detail.trim().is_empty())
    }
}

fn truncate_tail(detail: &str) -> String {
    let chars: Vec<char> = detail.chars().collect();
    if chars.len() <= SSH_STDERR_LIMIT {
        detail.to_owned()
    } else {
        chars[chars.len() - SSH_STDERR_LIMIT..].iter().collect()
    }
}

pub struct SshTunnel {
    endpoint: String,
    child: Arc<tokio::sync::Mutex<Option<Child>>>,
    failure: FailureSlot,
    /// Accept and pump tasks live on this runtime. It must outlive the tunnel:
    /// dropping a runtime cancels its tasks and invalidates its listeners.
    _runtime: tokio::runtime::Runtime,
}

impl SshTunnel {
    /// The loopback endpoint a WebSocket client dials instead of SSH.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Captured stderr or the exit reason, for authentication-failure copy.
    pub fn failure_detail(&self) -> Option<String> {
        self.failure.detail()
    }

    /// Kills the owned child. A reaped or exited child is simply absent from
    /// the slot, so no signal ever reaches an invalid or reused pid.
    ///
    /// Spawning a small runtime on this thread keeps calls from inside another
    /// runtime from nesting or dropping a runtime inside an async context.
    pub fn close(&self) {
        let child = self.child.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            if let Ok(runtime) = runtime {
                runtime.block_on(async {
                    if let Some(mut child) = child.lock().await.take() {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                    }
                });
            }
        });
    }
}

impl Drop for SshTunnel {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.try_lock()
            && let Some(mut child) = child.take()
        {
            let _ = child.start_kill();
        }
    }
}

pub fn open(target: &SshTransportTarget) -> Result<SshTunnel> {
    #[cfg(test)]
    if let Some((program, path_prepend)) = test_program() {
        return open_command(program, path_prepend, target);
    }
    open_command("ssh".to_owned(), None, target)
}

/// Test hook for the production [`open`] path: a stub must stand in for ssh
/// without changing the process-wide PATH that other tests share.
#[cfg(test)]
static TEST_PROGRAM: std::sync::Mutex<Option<(String, Option<String>)>> =
    std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_program(program: Option<(String, Option<String>)>) {
    *TEST_PROGRAM.lock().expect("test ssh program lock") = program;
}

/// The stub-program hook is process-global, so tests that start real tunnels
/// through it take this lock to keep their children from swapping mid-dial.
#[cfg(test)]
pub(crate) fn test_dial_lock() -> std::sync::MutexGuard<'static, ()> {
    TEST_DIAL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
static TEST_DIAL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
fn test_program() -> Option<(String, Option<String>)> {
    TEST_PROGRAM.lock().expect("test ssh program lock").clone()
}

/// Binds one local listener for the tunnel's lifetime. Every accepted
/// connection spawns a fresh ssh child — the shared connector's
/// per-connection process model — so reconnects through the same endpoint
/// each get a working tunnel. The first dial keeps a deadline so an unused
/// tunnel reports honestly; later accepts wait because reconnects arrive on
/// the supervising client's backoff schedule.
pub(crate) fn open_command(
    program: String,
    path_prepend: Option<String>,
    target: &SshTransportTarget,
) -> Result<SshTunnel> {
    let args = build_ssh_tunnel_args(target)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("paseo-ssh-tunnel")
        .enable_all()
        .build()
        .context("start the SSH tunnel runtime")?;
    let listener = runtime
        .block_on(TcpListener::bind("127.0.0.1:0"))
        .context("allocate the SSH tunnel port")?;
    let port = listener.local_addr().context("SSH tunnel port")?.port();
    let failure = FailureSlot::default();
    let child_slot = Arc::new(tokio::sync::Mutex::new(None));

    let accept_failure = failure.clone();
    let accept_slot = child_slot.clone();
    runtime.spawn(async move {
        let mut accepted = match tokio::time::timeout(HANDSHAKE_TIMEOUT, listener.accept()).await {
            Ok(Ok(accepted)) => Some(accepted),
            Ok(Err(error)) => {
                accept_failure.set_if_absent(format!("SSH tunnel accept failed: {error}"));
                None
            }
            Err(_) => {
                accept_failure
                    .set_if_absent("SSH tunnel was not used before its deadline".to_owned());
                None
            }
        };
        while let Some(connection) = accepted {
            // One HostClient owns this tunnel. Reap the previous child before
            // accepting a reconnect so an old pump cannot take the new child.
            forward(
                program.clone(),
                args.clone(),
                path_prepend.clone(),
                connection,
                accept_slot.clone(),
                accept_failure.clone(),
            )
            .await;
            accepted = match listener.accept().await {
                Ok(accepted) => Some(accepted),
                Err(error) => {
                    accept_failure.set_if_absent(format!("SSH tunnel accept failed: {error}"));
                    None
                }
            };
        }
    });

    Ok(SshTunnel {
        endpoint: format!("127.0.0.1:{port}"),
        child: child_slot,
        failure,
        _runtime: runtime,
    })
}

/// One forwarded connection: spawns a fresh ssh child, pumps bytes between
/// the accepted socket and the child's stdio, then reaps the child and
/// captures why it died.
async fn forward(
    program: String,
    args: Vec<String>,
    path_prepend: Option<String>,
    accepted: (tokio::net::TcpStream, std::net::SocketAddr),
    child_slot: Arc<tokio::sync::Mutex<Option<Child>>>,
    failure: FailureSlot,
) {
    let mut command = Command::new(program);
    command
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // CREATE_NO_WINDOW from the process-creation API, kept local so one
        // constant does not add a dependency crate.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
    }
    if let Some(prepend) = path_prepend {
        let existing = std::env::var("PATH").unwrap_or_default();
        command.env("PATH", format!("{prepend}:{existing}"));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            failure.set_if_absent(format!(
                "Cannot start ssh: {error}. Check that ssh is installed."
            ));
            return;
        }
    };
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        failure.set_if_absent("ssh process streams are unavailable".to_owned());
        return;
    };
    *child_slot.lock().await = Some(child);
    pump(accepted, stdin, stdout, stderr, child_slot, failure).await;
}

/// Copies bytes between the accepted local socket and the ssh child's stdio
/// until either side closes; then reaps the child and captures why it died.
async fn pump(
    accepted: (tokio::net::TcpStream, std::net::SocketAddr),
    mut stdin: tokio::process::ChildStdin,
    mut stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    child_slot: Arc<tokio::sync::Mutex<Option<Child>>>,
    failure: FailureSlot,
) {
    let stderr_task = tokio::spawn(collect_stderr(stderr));
    let (mut socket_reader, mut socket_writer) = tokio::io::split(accepted.0);
    let mut upstream = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let _ = tokio::io::copy(&mut socket_reader, &mut stdin).await;
        let _ = stdin.shutdown().await;
    });
    let mut downstream = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stdout, &mut socket_writer).await;
    });
    // Either EOF ends this connection. Drop does not cancel a JoinHandle;
    // abort and join the other pump before accepting the next connection.
    tokio::select! {
        _ = &mut downstream => {
            upstream.abort();
            let _ = upstream.await;
        }
        _ = &mut upstream => {
            downstream.abort();
            let _ = downstream.await;
        }
    }
    // Reap the child and capture why it died. An ssh process that is still
    // running is killed, not abandoned; an already-exited child is just reaped.
    if let Some(mut child) = child_slot.lock().await.take() {
        let _ = child.start_kill();
        let status = child.wait().await;
        let detail = truncate_tail(&stderr_task.await.unwrap_or_default());
        if !status.as_ref().is_ok_and(|status| status.success()) {
            if detail.trim().is_empty() {
                failure.set_if_absent(match status {
                    Ok(status) => format!("ssh exited with status {status}"),
                    Err(error) => format!("ssh failed: {error}"),
                });
            } else {
                failure.set_if_absent(detail);
            }
        }
    }
}

async fn collect_stderr(mut stderr: tokio::process::ChildStderr) -> String {
    use tokio::io::AsyncReadExt;
    let mut collected: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(len) => {
                collected.extend_from_slice(&chunk[..len]);
                if collected.len() > SSH_STDERR_LIMIT * 2 {
                    let extra = collected.len() - SSH_STDERR_LIMIT;
                    collected.drain(..extra);
                }
            }
        }
    }
    String::from_utf8_lossy(&collected).into_owned()
}

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnels_bytes_through_the_spawned_ssh_process() {
        use std::io::{Read as _, Write as _};
        let stub_dir = std::env::temp_dir().join(format!(
            "paseo-ssh-stub-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&stub_dir).unwrap();
        let stub = stub_dir.join("ssh");
        std::fs::write(&stub, "#!/bin/sh\ncat\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let target = SshTransportTarget {
            host: "stub-host".into(),
            ssh_port: None,
            daemon_port: 6767,
        };
        let tunnel = open_command(
            stub.to_string_lossy().into_owned(),
            Some(stub_dir.to_string_lossy().into_owned()),
            &target,
        )
        .unwrap();

        // Dial the tunnel like a WebSocket client would and echo bytes through it.
        let mut stream = std::net::TcpStream::connect(tunnel.endpoint()).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        stream.write_all(b"hello-tunnel").unwrap();
        // Half-close like a completed request so the stub's stdout reaches EOF.
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut buffer = Vec::new();
        stream.read_to_end(&mut buffer).unwrap();
        assert_eq!(buffer, b"hello-tunnel");
        drop(tunnel);
        let _ = std::fs::remove_dir_all(&stub_dir);
    }

    #[test]
    fn a_tunnel_accepts_again_after_a_forwarded_connection_ends() {
        use std::io::{Read as _, Write as _};
        let stub_dir = std::env::temp_dir().join(format!(
            "paseo-ssh-stub-loop-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&stub_dir).unwrap();
        let stub = stub_dir.join("ssh");
        std::fs::write(&stub, "#!/bin/sh\ncat\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let target = SshTransportTarget {
            host: "stub-host".into(),
            ssh_port: None,
            daemon_port: 6767,
        };
        let tunnel = open_command(
            stub.to_string_lossy().into_owned(),
            Some(stub_dir.to_string_lossy().into_owned()),
            &target,
        )
        .unwrap();

        // The first forwarded connection ends like a closed session; the
        // endpoint must still spawn a fresh ssh child for the next dial.
        for exchange in ["first-dial", "second-dial"] {
            let mut stream = std::net::TcpStream::connect(tunnel.endpoint()).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            stream.write_all(exchange.as_bytes()).unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut buffer = Vec::new();
            stream.read_to_end(&mut buffer).unwrap();
            assert_eq!(buffer, exchange.as_bytes());
        }
        drop(tunnel);
        let _ = std::fs::remove_dir_all(&stub_dir);
    }

    #[test]
    fn a_failing_stub_reports_its_stderr_detail() {
        let stub_dir = std::env::temp_dir().join(format!(
            "paseo-ssh-stub-fail-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&stub_dir).unwrap();
        let stub = stub_dir.join("ssh");
        std::fs::write(&stub, "#!/bin/sh\necho 'Permission denied' >&2\nexit 255\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let target = SshTransportTarget {
            host: "stub-host".into(),
            ssh_port: None,
            daemon_port: 6767,
        };
        let tunnel = open_command(
            stub.to_string_lossy().into_owned(),
            Some(stub_dir.to_string_lossy().into_owned()),
            &target,
        )
        .unwrap();
        // The dial spawns and then ends the process; the failure detail lands
        // once the child is reaped.
        let _dial = std::net::TcpStream::connect(tunnel.endpoint()).unwrap();
        for _ in 0..50 {
            if tunnel.failure_detail().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            tunnel
                .failure_detail()
                .is_some_and(|detail| detail.contains("Permission denied"))
        );
        drop(tunnel);
        let _ = std::fs::remove_dir_all(&stub_dir);
    }

    #[test]
    fn truncates_stderr_to_the_shared_tail_limit() {
        let detail = "x".repeat(SSH_STDERR_LIMIT + 10);
        let truncated = truncate_tail(&detail);
        assert_eq!(truncated.chars().count(), SSH_STDERR_LIMIT);
    }
}
