//! Native saved-host registry persistence.
//!
//! The native client's saved connections are its own list — separate from the
//! daemon's `hosts.json` and from the Expo app's storage. Host passwords are
//! client credentials and are never written to a remote host.
//!
//! Known limitation, kept visible on purpose: without OS keychain support in
//! this Rust package the file stores passwords as plaintext JSON with mode
//! `0600` on POSIX systems. The add-host UI states this where a password is
//! saved.

use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::profile::{
    HostProfile, StoredHostProfile, normalize_stored_host_profile, serialize_registry,
};

pub const PLAINTEXT_CREDENTIAL_LIMITATION: &str = "Saved host passwords are stored in this client's local registry file. Move OS keychain storage here before treating this device as untrusted.";

/// The native client's own state root, shared with future native settings and
/// workspace-remembered state. Lives beside the daemon home it discovers by
/// default, but is owned by the client.
pub fn state_root(home: &std::path::Path) -> PathBuf {
    home.join("native-client")
}

pub fn registry_path(home: &std::path::Path) -> PathBuf {
    state_root(home).join("hosts.json")
}

pub fn load(home: &std::path::Path) -> Result<Vec<HostProfile>> {
    let path = registry_path(home);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read the native host registry"),
    };
    // An unreadable registry is not an empty registry: reporting it separately
    // keeps the caller from replacing saved hosts with an empty list.
    let entries: Vec<serde_json::Value> =
        serde_json::from_slice(&bytes).context("the native host registry is not valid JSON")?;
    Ok(normalize_registry_entries(entries))
}

/// Invalid entries are dropped the way the app normalizes stored records:
/// each array element is validated on its own, so one bad entry never
/// discards the rest of the registry.
pub fn parse_registry(bytes: &[u8]) -> Vec<HostProfile> {
    let entries: std::result::Result<Vec<serde_json::Value>, _> = serde_json::from_slice(bytes);
    let Ok(entries) = entries else {
        return Vec::new();
    };
    normalize_registry_entries(entries)
}

fn normalize_registry_entries(entries: Vec<serde_json::Value>) -> Vec<HostProfile> {
    entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<StoredHostProfile>(entry).ok())
        .filter_map(|stored| normalize_stored_host_profile(&stored))
        .collect()
}

/// Atomic replace so a crash never truncates the registry. The temporary file
/// is created with mode 0600 before any credential bytes are written; POSIX
/// umasks cannot expose it first.
pub fn save(home: &std::path::Path, profiles: &[HostProfile]) -> Result<()> {
    let root = state_root(home);
    ensure_private_root(&root)?;
    let path = registry_path(home);
    let temporary = root.join(format!("hosts.json.tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .context("create the native host registry")?;
        use std::io::Write as _;
        file.write_all(&serialize_registry(profiles)?)
            .context("write the native host registry")?;
        file.sync_all().context("flush the native host registry")?;
        drop(file);
        std::fs::rename(&temporary, &path).context("replace the native host registry")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn ensure_private_root(root: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        if !root.exists() {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder
                .create(root)
                .context("create the native client state root")?;
        }
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(root).context("create the native client state root")?;
    }
    Ok(())
}

pub fn remove_profile(profiles: &[HostProfile], server_id: &str) -> Vec<HostProfile> {
    profiles
        .iter()
        .filter(|profile| profile.server_id != server_id)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::connection::HostConnection;

    fn temp_home() -> PathBuf {
        let home = std::env::temp_dir().join(format!(
            "paseo-native-registry-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn tcp(endpoint: &str) -> HostConnection {
        HostConnection::DirectTcp {
            id: format!("direct:{endpoint}"),
            endpoint: endpoint.to_owned(),
            use_tls: None,
        }
    }

    #[test]
    fn round_trips_profiles_through_the_state_root() {
        let home = temp_home();
        let profile = HostProfile {
            server_id: "srv_a".into(),
            password: Some("secret".into()),
            label: "Build box".into(),
            appearance: None,
            lifecycle: None,
            connections: vec![tcp("localhost:6767")],
            preferred_connection_id: Some("direct:localhost:6767".into()),
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        };
        save(&home, &[profile.clone()]).unwrap();
        let loaded = load(&home).unwrap();
        assert_eq!(loaded, vec![profile.clone()]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(registry_path(&home))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // The password never leaves the client's own state root.
        assert!(registry_path(&home).starts_with(home.join("native-client")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_truncated_registry_reports_an_error_instead_of_looking_empty() {
        let home = temp_home();
        std::fs::create_dir_all(state_root(&home)).unwrap();
        std::fs::write(registry_path(&home), b"{not json").unwrap();
        let error = load(&home).unwrap_err().to_string();
        assert!(error.contains("not valid JSON"), "{error}");
        assert_eq!(std::fs::read(registry_path(&home)).unwrap(), b"{not json");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn saving_never_leaves_a_temporary_registry_file() {
        let home = temp_home();
        save(&home, &[]).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(state_root(&home))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn removing_a_profile_keeps_the_others() {
        let profiles = vec![
            HostProfile {
                server_id: "srv_a".into(),
                password: None,
                label: "srv_a".into(),
                appearance: None,
                lifecycle: None,
                connections: vec![tcp("localhost:6767")],
                preferred_connection_id: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
            HostProfile {
                server_id: "srv_b".into(),
                password: None,
                label: "srv_b".into(),
                appearance: None,
                lifecycle: None,
                connections: vec![HostConnection::RemoteSsh {
                    id: "ssh:build-box::".into(),
                    host: "build-box".into(),
                    ssh_port: None,
                    daemon_port: None,
                }],
                preferred_connection_id: None,
                created_at: String::new(),
                updated_at: String::new(),
            },
        ];
        let next = remove_profile(&profiles, "srv_a");
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].server_id, "srv_b");
    }
}
