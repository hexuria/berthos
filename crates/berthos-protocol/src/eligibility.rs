//! Eligibility vocabulary. Evaluation (fail-closed) lives in `berthos-node`.

use serde::{Deserialize, Serialize};

/// Image tag the doctor requires. Rebuild after changing labels.
pub const REQUIRED_GUEST_IMAGE: &str = "berthos-linux-desktop:v1";
/// Version label stamped on the guest image (`berthos.guest.version`).
pub const REQUIRED_GUEST_VERSION: &str = "v1";
/// Desktop stack the guest must declare (`berthos.desktop`).
pub const REQUIRED_DESKTOP_LABEL: &str = "xvfb-openbox-chromium";
/// Egress label the guest must declare (`berthos.egress.policy`).
pub const REQUIRED_EGRESS_POLICY: &str = "default-deny";
/// Minimum free vCPU a node must advertise.
pub const MIN_FREE_VCPU: u32 = 2;
/// Minimum free memory in GiB a node must advertise.
pub const MIN_FREE_MEM_GIB: u32 = 4;

/// What is being offered as a berth. The host desktop is never a class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NodeClass {
    /// Isolated Linux (v1) guest VM or container, not the host session.
    #[default]
    VmGuest,
    /// Dedicated server guest. Still a guest, never the metal desktop.
    DedicatedServer,
    /// Personal laptop / daily-driver. Always rejected.
    Laptop,
}

/// Physical or virtual chassis the operator attests. Fail-closed when unknown
/// and the node wants to be public.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Chassis {
    /// Always-on server or mini-PC that is not a daily-driver.
    Server,
    /// Hypervisor / VM host dedicated to guests.
    VmHost,
    /// Laptop or daily-driver. Cannot be a public node.
    Laptop,
    /// Operator did not attest. Public intent fails closed.
    #[default]
    Unknown,
}

/// Whether this node is only a private loopback outpost or wants to be listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Intent {
    /// Local loopback / operator's own agents. Still isolated guests only.
    #[default]
    Private,
    /// Participate as inventory. Listings themselves live in berth-market.
    Public,
}

/// Guest operating system. v1 public and default private is Linux only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GuestOs {
    /// Isolated Linux desktop guest. The only v1 SKU.
    #[default]
    Linux,
    /// Windows Home OEM. Not a public listing. Not a v1 guest.
    WindowsHomeOem,
    /// Windows Pro OEM. Not a public listing. Not a v1 guest.
    WindowsProOem,
    /// macOS. Public is out of scope for v1. Private Windows/Mac later.
    Macos,
}

/// How the guest reaches the network. Empty allowlist means no outbound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EgressPolicy {
    /// Default deny. Required.
    #[default]
    DefaultDeny,
    /// Default allow. Always a failed check.
    DefaultAllow,
}

/// Labels read from the required guest image. Absence is a failed check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestImage {
    /// Local image reference, e.g. `berthos-linux-desktop:v1`.
    pub name: String,
    /// `berthos.guest.version`.
    pub version_label: String,
    /// `berthos.desktop`.
    pub desktop_label: String,
    /// `berthos.egress.policy`.
    pub egress_label: String,
}

impl GuestImage {
    /// True when every required label matches this repo's v1 contract.
    pub fn matches_required(&self) -> bool {
        self.version_label == REQUIRED_GUEST_VERSION
            && self.desktop_label == REQUIRED_DESKTOP_LABEL
            && self.egress_label == REQUIRED_EGRESS_POLICY
    }
}

/// Stable identifiers for doctor rows. Warnings do not fail the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckId {
    /// `class=laptop` is rejected; only vm-guest or dedicated-server.
    Class,
    /// Listener must be loopback. Bind-all is rejected.
    Bind,
    /// Docker (or equivalent) must be running.
    Runtime,
    /// Labeled Linux desktop guest image must be present.
    GuestImage,
    /// Egress must be default-deny.
    Egress,
    /// Enough free vCPU and RAM.
    Capacity,
    /// Public nodes must advertise wired + always-on.
    Availability,
    /// Public nodes cannot be a laptop / daily-driver chassis.
    Chassis,
    /// v1 guests are Linux. Windows OEM and public macOS fail.
    GuestOs,
    /// Tunnel is optional. Missing is a warning, not a failure.
    Tunnel,
}

impl std::fmt::Display for CheckId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Class => "class",
            Self::Bind => "bind",
            Self::Runtime => "runtime",
            Self::GuestImage => "guest_image",
            Self::Egress => "egress",
            Self::Capacity => "capacity",
            Self::Availability => "availability",
            Self::Chassis => "chassis",
            Self::GuestOs => "guest_os",
            Self::Tunnel => "tunnel",
        };
        f.write_str(name)
    }
}

/// Outcome of one doctor row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Required check passed.
    Pass,
    /// Required check failed. The node is ineligible.
    Fail,
    /// Advisory. Does not change eligibility.
    Warn,
}

/// One row of a doctor report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorCheck {
    /// Which gate this row is.
    pub id: CheckId,
    /// Pass / fail / warn.
    pub status: CheckStatus,
    /// Human-readable detail. Safe to print; contains no secrets.
    pub detail: String,
}

/// Fail-closed doctor result. `eligible` is true only when no check failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Protocol version the evaluator used.
    pub protocol: String,
    /// Intent the operator asked the doctor to judge.
    pub intent: Intent,
    /// True only if every required check passed.
    pub eligible: bool,
    /// Individual rows, including warnings.
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Whether a specific required check failed.
    pub fn failed(&self, id: CheckId) -> bool {
        self.checks
            .iter()
            .any(|c| c.id == id && c.status == CheckStatus::Fail)
    }

    /// First failure detail for `id`, if any.
    pub fn failure_detail(&self, id: CheckId) -> Option<&str> {
        self.checks
            .iter()
            .find(|c| c.id == id && c.status == CheckStatus::Fail)
            .map(|c| c.detail.as_str())
    }
}

/// Observed or attested facts the evaluator consumes. Probe errors must be
/// represented as missing / false — never skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// Advertised berth class.
    pub class: NodeClass,
    /// Attested chassis.
    pub chassis: Chassis,
    /// Private loopback vs public participation.
    pub intent: Intent,
    /// Guest OS that would be leased.
    pub guest_os: GuestOs,
    /// True when the node process will bind a loopback address.
    pub bind_is_loopback: bool,
    /// Bind address as advertised (for the report text).
    pub bind_display: String,
    /// Docker daemon (or equivalent) is up.
    pub runtime_running: bool,
    /// Inspected guest image, if present.
    pub guest_image: Option<GuestImage>,
    /// Effective egress policy the node will apply to guests.
    pub egress: EgressPolicy,
    /// Free vCPU the operator can give a guest.
    pub free_vcpu: u32,
    /// Free memory in GiB.
    pub free_mem_gib: u32,
    /// Operator attests the box does not sleep / lid-close.
    pub always_on: bool,
    /// Operator attests Ethernet / always-on uplink, not a cafe Wi-Fi laptop.
    pub wired: bool,
    /// Optional tunnel binary or config is present.
    pub tunnel_present: bool,
}
