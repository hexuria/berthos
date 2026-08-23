//! Isolated Linux desktop guest lifecycle.
//!
//! v1 revert is **destroy-and-recreate**. Snapshot/restore is the intended
//! later path; ending a lease always tears the container down so the next
//! tenant cannot inherit disk, processes, or cookies.
//!
//! Secrets stay on the node. The guest is started with `--network none`
//! (default-deny egress) and no env, no binds of `~/.berthos`, no host
//! display, no `--network=host`.

use berthos_protocol::LeaseId;

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
}

/// Docker-backed guests. Default-deny via `--network none`.
#[derive(Debug, Clone)]
pub struct DockerGuest;

impl GuestRuntime for DockerGuest {
    fn start(&self, lease_id: &LeaseId, spec: &GuestSpec) -> Result<GuestHandle, GuestError> {
        let container = format!("berthos-{}", lease_id.0);
        let mem = format!("{}g", spec.mem_gib);
        let cpus = spec.vcpu.to_string();
        let status = std::process::Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &container,
                "--network",
                "none",
                "--memory",
                &mem,
                "--cpus",
                &cpus,
                "--label",
                "berthos.role=guest",
                "--label",
                &format!("berthos.lease={}", lease_id.0),
                // Isolation: never host net, never host display, never secrets.
                spec.image.as_str(),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .map_err(|e| GuestError::Runtime(e.to_string()))?;
        if !status.success() {
            return Err(GuestError::Runtime(format!(
                "docker run failed for {container}; is {REQUIRED} built?",
                REQUIRED = spec.image
            )));
        }
        Ok(GuestHandle { container })
    }

    fn destroy(&self, handle: &GuestHandle) -> Result<(), GuestError> {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &handle.container])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        Ok(())
    }
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
