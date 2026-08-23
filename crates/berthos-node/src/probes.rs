//! Host probes. Any error is treated as a failed fact (fail closed).

use std::process::Command;

use berthos_protocol::{
    Chassis, EgressPolicy, Facts, GuestImage, GuestOs, Intent, NodeClass, REQUIRED_GUEST_IMAGE,
};

use crate::config::NodeConfig;

/// True when `docker info` succeeds. Missing binary or a down daemon is false.
pub fn docker_available() -> bool {
    docker_running()
}

/// True when the required guest image is present **and** carries v1 labels.
pub fn labeled_guest_image_ready(name: &str) -> bool {
    docker_available()
        && inspect_guest_image(name)
            .map(|image| image.matches_required())
            .unwrap_or(false)
}

/// Run live probes and fold them into [`Facts`]. Probe failures become
/// conservative (false / missing / zero) values — never "skip this check".
pub fn observe(config: &NodeConfig) -> Facts {
    let runtime_running = docker_running();
    let guest_image = if runtime_running {
        inspect_guest_image(&config.image)
    } else {
        None
    };

    Facts {
        class: config.class,
        chassis: config.chassis,
        intent: config.intent,
        guest_os: config.guest_os,
        bind_is_loopback: config.bind_ip.is_loopback(),
        bind_display: format!("{}:{}", config.bind_ip, config.port),
        runtime_running,
        guest_image,
        egress: config.egress,
        free_vcpu: free_vcpu(),
        free_mem_gib: free_mem_gib(),
        always_on: config.always_on,
        wired: config.wired,
        tunnel_present: tunnel_present(),
    }
}

/// Facts for `--simulate` smoke cases. These never produce a green public node
/// except the documented `eligible-private` fixture used by unit tests.
pub fn simulate(case: SimulateCase) -> Facts {
    let mut facts = crate::eligibility::eligible_private_facts();
    match case {
        SimulateCase::Laptop => facts.class = NodeClass::Laptop,
        SimulateCase::MissingImage => facts.guest_image = None,
        SimulateCase::BindAll => {
            facts.bind_is_loopback = false;
            facts.bind_display = "0.0.0.0:7432".into();
        }
    }
    facts
}

/// Fail-closed doctor simulations. Success cannot be simulated from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulateCase {
    /// `class=laptop`.
    Laptop,
    /// No labeled guest image.
    MissingImage,
    /// Listener on all interfaces.
    BindAll,
}

impl SimulateCase {
    /// Parse a CLI `--simulate` value.
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "laptop" => Ok(Self::Laptop),
            "missing-image" => Ok(Self::MissingImage),
            "bind-all" => Ok(Self::BindAll),
            other => Err(format!(
                "unknown simulate case {other:?}; expected laptop, missing-image, or bind-all"
            )),
        }
    }
}

fn docker_running() -> bool {
    Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn inspect_guest_image(name: &str) -> Option<GuestImage> {
    let output = Command::new("docker")
        .args([
            "image",
            "inspect",
            name,
            "--format",
            "{{json .Config.Labels}}",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let labels: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(GuestImage {
        name: name.to_string(),
        version_label: labels
            .get("berthos.guest.version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        desktop_label: labels
            .get("berthos.desktop")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        egress_label: labels
            .get("berthos.egress.policy")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

fn free_vcpu() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(0)
}

fn free_mem_gib() -> u32 {
    if let Ok(text) = std::fs::read_to_string("/proc/meminfo") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let kb: u64 = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                return (kb / 1024 / 1024) as u32;
            }
        }
        return 0;
    }
    // Non-Linux: fail closed on memory rather than invent a number.
    0
}

fn tunnel_present() -> bool {
    Command::new("cloudflared")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Defaults used when no node config file exists.
pub fn default_facts_config() -> NodeConfig {
    NodeConfig {
        class: NodeClass::VmGuest,
        chassis: Chassis::Unknown,
        intent: Intent::Private,
        guest_os: GuestOs::Linux,
        bind_ip: "127.0.0.1".parse().expect("loopback"),
        port: 7432,
        egress: EgressPolicy::DefaultDeny,
        always_on: false,
        wired: false,
        image: REQUIRED_GUEST_IMAGE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulate_cases_are_fail_closed_only() {
        for case in [
            SimulateCase::Laptop,
            SimulateCase::MissingImage,
            SimulateCase::BindAll,
        ] {
            let report = crate::eligibility::evaluate(&simulate(case));
            assert!(!report.eligible, "{case:?} must not be eligible");
        }
    }

    #[test]
    fn simulate_rejects_eligible_keyword() {
        assert!(SimulateCase::parse("eligible").is_err());
        assert!(SimulateCase::parse("pass").is_err());
    }

    #[test]
    fn live_docker_probe_skips_when_daemon_missing() {
        if docker_available() {
            let facts = observe(&default_facts_config());
            assert!(
                facts.runtime_running,
                "docker info succeeded so runtime_running must be true"
            );
            return;
        }
        let facts = observe(&default_facts_config());
        assert!(
            !facts.runtime_running,
            "missing docker must fail closed, not skip"
        );
        assert!(facts.guest_image.is_none());
    }

    #[test]
    fn live_docker_requires_labeled_guest_image() {
        if !docker_available() {
            eprintln!("skip live image probe: docker daemon not available");
            return;
        }
        let facts = observe(&default_facts_config());
        assert!(facts.runtime_running);
        match &facts.guest_image {
            None => {
                let report = crate::eligibility::evaluate(&facts);
                assert!(!report.ok);
                assert!(report.failed(berthos_protocol::CheckId::GuestImage));
            }
            Some(image) if image.matches_required() => {
                assert_eq!(image.name, REQUIRED_GUEST_IMAGE);
                assert_eq!(
                    image.version_label,
                    berthos_protocol::REQUIRED_GUEST_VERSION
                );
                assert_eq!(
                    image.desktop_label,
                    berthos_protocol::REQUIRED_DESKTOP_LABEL
                );
                assert_eq!(image.egress_label, berthos_protocol::REQUIRED_EGRESS_POLICY);
            }
            Some(image) => {
                let report = crate::eligibility::evaluate(&facts);
                assert!(!report.ok, "unlabeled image must fail closed: {image:?}");
                assert!(report.failed(berthos_protocol::CheckId::GuestImage));
            }
        }
    }
}
