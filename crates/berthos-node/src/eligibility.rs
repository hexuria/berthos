//! Fail-closed eligibility gate.
//!
//! A missing probe, a laptop class, a bind-all listener, or an unlabeled
//! guest image is a **failure**, never a skip. Warnings (optional tunnel)
//! cannot make an ineligible node eligible.

use berthos_protocol::{
    Chassis, CheckId, CheckStatus, DoctorCheck, DoctorReport, EgressPolicy, Facts, GuestOs,
    ImageAttestation, Intent, NodeClass, ATTESTATION_SOURCE, MIN_FREE_MEM_GIB, MIN_FREE_VCPU,
    PROTOCOL_VERSION, REQUIRED_DESKTOP_LABEL, REQUIRED_EGRESS_POLICY, REQUIRED_GUEST_IMAGE,
    REQUIRED_GUEST_VERSION,
};
use time::OffsetDateTime;

/// Evaluate attested + observed facts. Fail closed.
pub fn evaluate(facts: &Facts) -> DoctorReport {
    evaluate_at(facts, OffsetDateTime::now_utc())
}

/// Evaluate with a fixed timestamp (tests / replay).
pub fn evaluate_at(facts: &Facts, timestamp: OffsetDateTime) -> DoctorReport {
    let checks = vec![
        class_check(facts.class),
        bind_check(facts.bind_is_loopback, &facts.bind_display),
        runtime_check(facts.runtime_running),
        guest_image_check(facts),
        egress_check(facts.egress),
        capacity_check(facts.free_vcpu, facts.free_mem_gib),
        guest_os_check(facts.guest_os, facts.intent),
        chassis_check(facts.chassis, facts.intent),
        availability_check(facts.always_on, facts.wired, facts.intent),
        tunnel_check(facts.tunnel_present),
    ];

    let eligible = checks.iter().all(|c| c.status != CheckStatus::Fail);
    DoctorReport {
        protocol: PROTOCOL_VERSION.to_string(),
        source: ATTESTATION_SOURCE.to_string(),
        ok: eligible,
        eligible,
        class: facts.class,
        intent: facts.intent,
        checks,
        image: facts.guest_image.as_ref().map(ImageAttestation::from),
        timestamp,
    }
}

fn class_check(class: NodeClass) -> DoctorCheck {
    match class {
        NodeClass::Laptop => fail(
            CheckId::Class,
            "class=laptop is rejected; only a VM guest or a dedicated server guest may be leased",
        ),
        NodeClass::VmGuest => pass(
            CheckId::Class,
            "class=vm-guest (isolated guest, not the host desktop)",
        ),
        NodeClass::DedicatedServer => pass(
            CheckId::Class,
            "class=dedicated-server (guest on dedicated metal, not the host desktop)",
        ),
    }
}

fn bind_check(is_loopback: bool, display: &str) -> DoctorCheck {
    if is_loopback {
        pass(CheckId::Bind, format!("loopback bind only ({display})"))
    } else {
        fail(
            CheckId::Bind,
            format!("bind-all rejected ({display}); node HTTP must listen on 127.0.0.1 / ::1"),
        )
    }
}

fn runtime_check(running: bool) -> DoctorCheck {
    if running {
        pass(CheckId::Runtime, "Docker (or equivalent) is running")
    } else {
        fail(
            CheckId::Runtime,
            "Docker (or equivalent) is not running; guests cannot be isolated",
        )
    }
}

fn guest_image_check(facts: &Facts) -> DoctorCheck {
    match &facts.guest_image {
        None => fail(
            CheckId::GuestImage,
            format!(
                "labeled Linux desktop guest image {REQUIRED_GUEST_IMAGE} is missing; build images/linux-desktop"
            ),
        ),
        Some(image) if image.matches_required() => pass(
            CheckId::GuestImage,
            format!(
                "{} labels ok ({REQUIRED_GUEST_VERSION}, {REQUIRED_DESKTOP_LABEL}, {REQUIRED_EGRESS_POLICY})",
                image.name
            ),
        ),
        Some(image) => fail(
            CheckId::GuestImage,
            format!(
                "image {} is present but labels do not match v1 (need berthos.guest.version={REQUIRED_GUEST_VERSION}, berthos.desktop={REQUIRED_DESKTOP_LABEL}, berthos.egress.policy={REQUIRED_EGRESS_POLICY}); rebuild",
                image.name
            ),
        ),
    }
}

fn egress_check(policy: EgressPolicy) -> DoctorCheck {
    match policy {
        EgressPolicy::DefaultDeny => pass(
            CheckId::Egress,
            "default-deny egress (empty allowlist = no outbound, including DNS)",
        ),
        EgressPolicy::DefaultAllow => fail(
            CheckId::Egress,
            "default-allow egress is rejected; a desktop with a browser is a fraud appliance",
        ),
    }
}

fn capacity_check(free_vcpu: u32, free_mem_gib: u32) -> DoctorCheck {
    if free_vcpu == 0 || free_mem_gib == 0 {
        return fail(
            CheckId::Capacity,
            "vcpu or memory of 0 is rejected (not unlimited); capacity probe must return real free resources",
        );
    }
    if free_vcpu < MIN_FREE_VCPU || free_mem_gib < MIN_FREE_MEM_GIB {
        fail(
            CheckId::Capacity,
            format!(
                "not enough free capacity ({free_vcpu} vCPU, {free_mem_gib} GiB); need at least {MIN_FREE_VCPU} vCPU and {MIN_FREE_MEM_GIB} GiB"
            ),
        )
    } else {
        pass(
            CheckId::Capacity,
            format!("{free_vcpu} vCPU and {free_mem_gib} GiB free"),
        )
    }
}

fn guest_os_check(os: GuestOs, intent: Intent) -> DoctorCheck {
    match (os, intent) {
        (GuestOs::Linux, _) => pass(CheckId::GuestOs, "guest OS is Linux (v1 SKU)"),
        (GuestOs::WindowsHomeOem, Intent::Public) | (GuestOs::WindowsProOem, Intent::Public) => {
            fail(
                CheckId::GuestOs,
                "Windows Home/Pro OEM on the metal is not a public listing",
            )
        }
        (GuestOs::WindowsHomeOem, Intent::Private) | (GuestOs::WindowsProOem, Intent::Private) => {
            fail(
                CheckId::GuestOs,
                "v1 is Linux guest only; a private Windows VM for the operator's own agent is later, not now",
            )
        }
        (GuestOs::Macos, Intent::Public) => fail(
            CheckId::GuestOs,
            "public macOS is out of scope for v1",
        ),
        (GuestOs::Macos, Intent::Private) => fail(
            CheckId::GuestOs,
            "v1 is Linux guest only; private macOS is out of scope",
        ),
    }
}

fn chassis_check(chassis: Chassis, intent: Intent) -> DoctorCheck {
    match (chassis, intent) {
        (Chassis::Laptop, Intent::Public) => fail(
            CheckId::Chassis,
            "never accept a personal laptop or daily-driver as a public node",
        ),
        (Chassis::Unknown, Intent::Public) => fail(
            CheckId::Chassis,
            "public participation requires an attested chassis (server or vm-host); unknown fails closed",
        ),
        (Chassis::Laptop, Intent::Private) => pass(
            CheckId::Chassis,
            "laptop chassis allowed only as a private loopback *host*; the leased thing is still an isolated guest",
        ),
        (Chassis::Unknown, Intent::Private) => pass(
            CheckId::Chassis,
            "chassis unattested; acceptable for private loopback only",
        ),
        (Chassis::Server | Chassis::VmHost, _) => pass(
            CheckId::Chassis,
            "chassis is a dedicated host, not a daily-driver desktop",
        ),
    }
}

fn availability_check(always_on: bool, wired: bool, intent: Intent) -> DoctorCheck {
    if intent == Intent::Private {
        return pass(
            CheckId::Availability,
            "private loopback does not require wired/always-on (still required to go public)",
        );
    }
    if always_on && wired {
        pass(CheckId::Availability, "advertised wired and always-on")
    } else {
        fail(
            CheckId::Availability,
            format!(
                "public node must advertise wired+always-on (always_on={always_on}, wired={wired}); lid-close is not an SLA"
            ),
        )
    }
}

fn tunnel_check(present: bool) -> DoctorCheck {
    if present {
        pass(CheckId::Tunnel, "tunnel is present (optional)")
    } else {
        DoctorCheck {
            id: CheckId::Tunnel,
            status: CheckStatus::Warn,
            detail: "no tunnel configured; loopback-only is fine. remote access is a later optional tunnel, never bind-all".to_string(),
        }
    }
}

fn pass(id: CheckId, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        id,
        status: CheckStatus::Pass,
        detail: detail.into(),
    }
}

fn fail(id: CheckId, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        id,
        status: CheckStatus::Fail,
        detail: detail.into(),
    }
}

/// Fixture that would pass a private-intent doctor. Tests mutate one field.
pub fn eligible_private_facts() -> Facts {
    Facts {
        class: NodeClass::VmGuest,
        chassis: Chassis::VmHost,
        intent: Intent::Private,
        guest_os: GuestOs::Linux,
        bind_is_loopback: true,
        bind_display: "127.0.0.1:7432".to_string(),
        runtime_running: true,
        guest_image: Some(berthos_protocol::GuestImage {
            name: REQUIRED_GUEST_IMAGE.to_string(),
            version_label: REQUIRED_GUEST_VERSION.to_string(),
            desktop_label: REQUIRED_DESKTOP_LABEL.to_string(),
            egress_label: REQUIRED_EGRESS_POLICY.to_string(),
        }),
        egress: EgressPolicy::DefaultDeny,
        free_vcpu: MIN_FREE_VCPU,
        free_mem_gib: MIN_FREE_MEM_GIB,
        always_on: true,
        wired: true,
        tunnel_present: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn laptop_class_rejected() {
        let mut facts = eligible_private_facts();
        facts.class = NodeClass::Laptop;
        let report = evaluate(&facts);
        assert!(!report.eligible, "laptop must fail closed");
        assert!(report.failed(CheckId::Class));
        assert!(report
            .failure_detail(CheckId::Class)
            .unwrap()
            .contains("class=laptop"));
    }

    #[test]
    fn missing_image_rejected() {
        let mut facts = eligible_private_facts();
        facts.guest_image = None;
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::GuestImage));
        assert!(report
            .failure_detail(CheckId::GuestImage)
            .unwrap()
            .contains("missing"));
    }

    #[test]
    fn unlabeled_image_rejected() {
        let mut facts = eligible_private_facts();
        facts.guest_image = Some(berthos_protocol::GuestImage {
            name: "old-desktop:dev".into(),
            version_label: String::new(),
            desktop_label: "something".into(),
            egress_label: String::new(),
        });
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::GuestImage));
    }

    #[test]
    fn bind_all_rejected() {
        let mut facts = eligible_private_facts();
        facts.bind_is_loopback = false;
        facts.bind_display = "0.0.0.0:7432".into();
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::Bind));
        assert!(report
            .failure_detail(CheckId::Bind)
            .unwrap()
            .contains("bind-all"));
    }

    #[test]
    fn eligible_private_fixture_passes() {
        let report = evaluate(&eligible_private_facts());
        assert!(report.eligible, "{report:?}");
        assert!(!report.failed(CheckId::Tunnel));
        assert!(report
            .checks
            .iter()
            .any(|c| c.id == CheckId::Tunnel && c.status == CheckStatus::Warn));
    }

    #[test]
    fn public_laptop_chassis_rejected() {
        let mut facts = eligible_private_facts();
        facts.intent = Intent::Public;
        facts.chassis = Chassis::Laptop;
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::Chassis));
    }

    #[test]
    fn public_windows_oem_rejected() {
        let mut facts = eligible_private_facts();
        facts.intent = Intent::Public;
        facts.guest_os = GuestOs::WindowsHomeOem;
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::GuestOs));
    }

    #[test]
    fn public_macos_rejected() {
        let mut facts = eligible_private_facts();
        facts.intent = Intent::Public;
        facts.guest_os = GuestOs::Macos;
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::GuestOs));
    }

    #[test]
    fn default_allow_egress_rejected() {
        let mut facts = eligible_private_facts();
        facts.egress = EgressPolicy::DefaultAllow;
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::Egress));
    }

    #[test]
    fn missing_runtime_rejected() {
        let mut facts = eligible_private_facts();
        facts.runtime_running = false;
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::Runtime));
    }

    #[test]
    fn public_requires_wired_always_on() {
        let mut facts = eligible_private_facts();
        facts.intent = Intent::Public;
        facts.always_on = false;
        facts.wired = false;
        let report = evaluate(&facts);
        assert!(!report.eligible);
        assert!(report.failed(CheckId::Availability));
    }

    #[test]
    fn attestation_includes_ok_class_image_labels_and_timestamp() {
        let report = evaluate(&eligible_private_facts());
        assert!(report.ok);
        assert_eq!(report.ok, report.eligible);
        assert_eq!(report.class, NodeClass::VmGuest);
        assert_eq!(report.source, ATTESTATION_SOURCE);
        let image = report.image.expect("labeled fixture image");
        assert_eq!(image.name, REQUIRED_GUEST_IMAGE);
        assert_eq!(image.labels.guest_version, REQUIRED_GUEST_VERSION);
        assert_eq!(image.labels.desktop, REQUIRED_DESKTOP_LABEL);
        assert_eq!(image.labels.egress_policy, REQUIRED_EGRESS_POLICY);
        assert!(report.timestamp.year() >= 2026);
    }

    #[test]
    fn missing_image_attestation_is_null_and_not_ok() {
        let mut facts = eligible_private_facts();
        facts.guest_image = None;
        let report = evaluate(&facts);
        assert!(!report.ok);
        assert!(report.image.is_none());
    }
}
