//! Isolated Linux desktop guest lifecycle.
//!
//! v1 revert is **destroy-and-recreate**. Snapshot/restore is the intended
//! later path; ending a lease always tears the container down so the next
//! tenant cannot inherit disk, processes, or cookies.
//!
//! Secrets stay on the node. The guest is started with `--network none`
//! (default-deny egress) and no env, no binds of `~/.berthos`, no host
//! display, no `--network=host`.

use std::process::{Command, Stdio};

use berthos_protocol::LeaseId;
use serde_json::Value;

/// What to boot. No credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSpec {
    /// Image reference (`berthos-linux-desktop:v1`).
    pub image: String,
    /// vCPU limit.
    pub vcpu: u32,
    /// Memory limit in GiB.
    pub mem_gib: u32,
}

/// Handle recorded while a guest is live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestHandle {
    /// Docker (or fake) container name.
    pub container: String,
}

/// Observed isolation of a running guest. Used to fail closed if Docker
/// somehow applied host network, host display, or a privileged box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestIsolation {
    /// Docker `HostConfig.NetworkMode`. Must be `none`.
    pub network_mode: String,
    /// Privileged containers are rejected.
    pub privileged: bool,
    /// `PidMode`. `host` is rejected.
    pub pid_mode: String,
    /// `IpcMode`. `host` is rejected.
    pub ipc_mode: String,
    /// Bind mounts. Host display sockets are rejected.
    pub binds: Vec<String>,
    /// Other mounts (destination or source).
    pub mounts: Vec<String>,
}

impl GuestIsolation {
    /// True when the guest cannot see host net, host display, or host PID/IPC.
    pub fn is_isolated(&self) -> bool {
        self.network_mode == "none"
            && !self.privileged
            && self.pid_mode != "host"
            && self.ipc_mode != "host"
            && !self.exposes_host_display()
    }

    fn exposes_host_display(&self) -> bool {
        self.binds
            .iter()
            .chain(self.mounts.iter())
            .any(|p| path_is_host_display(p))
    }
}

fn path_is_host_display(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains(".x11-unix") || lower.contains("/tmp/.x11") || lower.contains("wayland")
}

/// How a guest is started and destroyed.
pub trait GuestRuntime: Send + Sync {
    /// Start an isolated guest. Must not mount secrets.
    fn start(&self, lease_id: &LeaseId, spec: &GuestSpec) -> Result<GuestHandle, GuestError>;
    /// Destroy the guest. v1 "revert".
    fn destroy(&self, handle: &GuestHandle) -> Result<(), GuestError>;
}

/// Guest runtime errors.
#[derive(Debug, thiserror::Error)]
pub enum GuestError {
    /// Runtime failed to start or destroy.
    #[error("guest runtime: {0}")]
    Runtime(String),
    /// Started container violated the isolation contract.
    #[error("guest isolation: {0}")]
    Isolation(String),
}

/// `docker run` argv after the binary name. Kept as a function so tests can
/// assert the isolation flags without talking to a daemon.
pub fn isolated_run_args(lease_id: &LeaseId, spec: &GuestSpec) -> (String, Vec<String>) {
    let container = format!("berthos-{}", lease_id.0);
    let mem = format!("{}g", spec.mem_gib);
    let cpus = spec.vcpu.to_string();
    let lease_label = format!("berthos.lease={}", lease_id.0);
    let args = vec![
        "run".into(),
        "-d".into(),
        "--rm".into(),
        "--name".into(),
        container.clone(),
        "--network".into(),
        "none".into(),
        "--memory".into(),
        mem,
        "--cpus".into(),
        cpus,
        "--label".into(),
        "berthos.role=guest".into(),
        "--label".into(),
        lease_label,
        spec.image.clone(),
    ];
    (container, args)
}

/// True when `args` would expose the host desktop, host cursor, or host net.
pub fn args_violate_isolation(args: &[String]) -> bool {
    let joined = args.join(" ");
    if joined.contains("--network host") || joined.contains("--network=host") {
        return true;
    }
    if args.iter().any(|a| a == "--privileged") {
        return true;
    }
    if args.iter().any(|a| a == "-e" || a.starts_with("--env")) {
        // Host DISPLAY / secrets must never be injected. The image sets its own DISPLAY.
        return true;
    }
    if args
        .iter()
        .any(|a| a == "-v" || a.starts_with("--volume") || a == "--mount")
    {
        return true;
    }
    args.iter().any(|a| path_is_host_display(a))
}

/// Docker-backed guests. Default-deny via `--network none`.
#[derive(Debug, Clone)]
pub struct DockerGuest;

impl GuestRuntime for DockerGuest {
    fn start(&self, lease_id: &LeaseId, spec: &GuestSpec) -> Result<GuestHandle, GuestError> {
        let (container, args) = isolated_run_args(lease_id, spec);
        if args_violate_isolation(&args) {
            return Err(GuestError::Isolation(
                "refusing to start a guest with host net, host display, env, or binds".into(),
            ));
        }
        let output = Command::new("docker")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| GuestError::Runtime(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(GuestError::Runtime(format!(
                "docker run failed for {container}; is {} built? {stderr}",
                spec.image
            )));
        }
        let handle = GuestHandle {
            container: container.clone(),
        };
        match inspect_isolation(&handle) {
            Ok(isolation) if isolation.is_isolated() => Ok(handle),
            Ok(isolation) => {
                let _ = self.destroy(&handle);
                Err(GuestError::Isolation(format!(
                    "container {container} was not isolated (network_mode={}, privileged={})",
                    isolation.network_mode, isolation.privileged
                )))
            }
            Err(e) => {
                let _ = self.destroy(&handle);
                Err(e)
            }
        }
    }

    fn destroy(&self, handle: &GuestHandle) -> Result<(), GuestError> {
        let output = Command::new("docker")
            .args(["rm", "-f", &handle.container])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| GuestError::Runtime(e.to_string()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !container_already_gone(&stderr) {
                return Err(GuestError::Runtime(format!(
                    "docker rm -f {} failed: {stderr}",
                    handle.container
                )));
            }
        }
        Ok(())
    }
}

fn container_already_gone(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("no such container") || lower.contains("is not running")
}

/// Inspect a running container's isolation. Used by the live Docker tests
/// and by [`DockerGuest::start`] to fail closed.
pub fn inspect_isolation(handle: &GuestHandle) -> Result<GuestIsolation, GuestError> {
    let output = Command::new("docker")
        .args(["inspect", &handle.container])
        .output()
        .map_err(|e| GuestError::Runtime(e.to_string()))?;
    if !output.status.success() {
        return Err(GuestError::Runtime(format!(
            "docker inspect {} failed: {}",
            handle.container,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    isolation_from_inspect_json(&output.stdout)
}

fn isolation_from_inspect_json(bytes: &[u8]) -> Result<GuestIsolation, GuestError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| GuestError::Runtime(format!("inspect json: {e}")))?;
    let host = value
        .get(0)
        .and_then(|c| c.get("HostConfig"))
        .ok_or_else(|| GuestError::Runtime("inspect missing HostConfig".into()))?;
    let binds = string_array(host.get("Binds"));
    let mounts = host
        .get("Mounts")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .flat_map(|m| {
                    [
                        m.get("Source").and_then(|v| v.as_str()).unwrap_or(""),
                        m.get("Destination").and_then(|v| v.as_str()).unwrap_or(""),
                    ]
                })
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(GuestIsolation {
        network_mode: host
            .get("NetworkMode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        privileged: host
            .get("Privileged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        pid_mode: host
            .get("PidMode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        ipc_mode: host
            .get("IpcMode")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        binds,
        mounts,
    })
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// In-memory guest used by tests. Never talks to Docker.
#[derive(Debug, Default)]
pub struct MemoryGuest {
    /// Containers that were started and not yet destroyed.
    pub live: std::sync::Mutex<Vec<String>>,
}

impl GuestRuntime for MemoryGuest {
    fn start(&self, lease_id: &LeaseId, _spec: &GuestSpec) -> Result<GuestHandle, GuestError> {
        let container = format!("memory-{}", lease_id.0);
        self.live
            .lock()
            .expect("memory guest lock")
            .push(container.clone());
        Ok(GuestHandle { container })
    }

    fn destroy(&self, handle: &GuestHandle) -> Result<(), GuestError> {
        self.live
            .lock()
            .expect("memory guest lock")
            .retain(|c| c != &handle.container);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> GuestSpec {
        GuestSpec {
            image: berthos_protocol::REQUIRED_GUEST_IMAGE.to_string(),
            vcpu: 1,
            mem_gib: 1,
        }
    }

    #[test]
    fn isolated_run_args_use_network_none_and_no_host_display() {
        let id = LeaseId("l_test".into());
        let (container, args) = isolated_run_args(&id, &sample_spec());
        assert_eq!(container, "berthos-l_test");
        assert!(args.windows(2).any(|w| w == ["--network", "none"]));
        assert!(!args_violate_isolation(&args));
        let joined = args.join(" ");
        assert!(!joined.contains("host"));
        assert!(!joined.contains("X11"));
        assert!(!joined.contains("DISPLAY"));
        assert!(!joined.contains(".berthos"));
        assert!(args.iter().any(|a| a == "berthos.role=guest"));
    }

    #[test]
    fn isolation_predicate_rejects_host_net_and_x11() {
        let good = GuestIsolation {
            network_mode: "none".into(),
            privileged: false,
            pid_mode: String::new(),
            ipc_mode: String::new(),
            binds: vec![],
            mounts: vec![],
        };
        assert!(good.is_isolated());

        let host_net = GuestIsolation {
            network_mode: "host".into(),
            ..good.clone()
        };
        assert!(!host_net.is_isolated());

        let x11 = GuestIsolation {
            binds: vec!["/tmp/.X11-unix:/tmp/.X11-unix".into()],
            ..good.clone()
        };
        assert!(!x11.is_isolated());

        let privileged = GuestIsolation {
            privileged: true,
            ..good
        };
        assert!(!privileged.is_isolated());
    }

    #[test]
    fn isolation_from_inspect_json_reads_host_config() {
        let json = br#"[{
            "HostConfig": {
                "NetworkMode": "none",
                "Privileged": false,
                "PidMode": "",
                "IpcMode": "",
                "Binds": null,
                "Mounts": []
            }
        }]"#;
        let isolation = isolation_from_inspect_json(json).expect("parse");
        assert!(isolation.is_isolated());
    }
}
