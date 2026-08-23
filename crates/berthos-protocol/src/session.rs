//! Lease, quote, and receipt types. The meter is occupancy seconds, not clicks.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::GuestOs;

/// Default guest shape for v1 Linux isolated.
pub const DEFAULT_VCPU: u32 = 2;
/// Default guest memory.
pub const DEFAULT_MEM_GIB: u32 = 4;
/// Default guest boot disk.
pub const DEFAULT_DISK_GIB: u32 = 40;
/// Every lease meters at least this many seconds.
pub const DEFAULT_MIN_SECONDS: u64 = 60;
/// Notional USD/hour for the default shape. **Not charged in this repo.**
pub const DEFAULT_NOTIONAL_USD_PER_HOUR: &str = "0.048";

/// What a quote or receipt is counting. Clicks are not a unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccupancyUnit {
    /// Wall-clock seconds the guest is held. Idle costs the same as busy.
    Seconds,
}

/// Tenancy. v1 leases are isolated guests. Host desktop is never a density.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    /// One tenant, one guest. Default.
    Isolated,
}

/// Stable pointer: this crate quotes occupancy; it does not settle it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementHint {
    /// Always false in berthos. Wallets and x402 live in berth-market.
    pub charged_here: bool,
    /// Human-readable reminder for operators and agents.
    pub note: String,
}

impl SettlementHint {
    /// The only settlement hint this repo is allowed to emit.
    pub fn not_charged_here() -> Self {
        Self {
            charged_here: false,
            note: "quoted, not charged. listings and settlement live in https://github.com/hexuria/berth-market"
                .to_string(),
        }
    }
}

/// Occupancy quote printed before a lease starts. Not an invoice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quote {
    /// Requested vCPU. `0` is invalid (not unlimited).
    pub vcpu: u32,
    /// Requested memory in GiB. `0` is invalid.
    pub mem_gib: u32,
    /// Boot disk in GiB.
    pub disk_gib: u32,
    /// Guest OS. v1 accepts `linux` only.
    pub os: GuestOs,
    /// Isolated guest. Never the host desktop.
    pub density: Density,
    /// Minimum billed occupancy, in seconds.
    pub min_seconds: u64,
    /// Always [`OccupancyUnit::Seconds`].
    pub occupancy_unit: OccupancyUnit,
    /// Notional USD per hour for this shape. Informational.
    pub notional_usd_per_hour: String,
    /// Reminder that berthos does not take payment.
    pub settlement: SettlementHint,
}

impl Quote {
    /// Build a v1 Linux isolated quote. Rejects zero resources.
    pub fn linux_isolated(vcpu: u32, mem_gib: u32, disk_gib: u32) -> Result<Self, QuoteError> {
        if vcpu == 0 {
            return Err(QuoteError::ZeroVcpu);
        }
        if mem_gib == 0 {
            return Err(QuoteError::ZeroMemory);
        }
        if disk_gib == 0 {
            return Err(QuoteError::ZeroDisk);
        }
        Ok(Self {
            vcpu,
            mem_gib,
            disk_gib,
            os: GuestOs::Linux,
            density: Density::Isolated,
            min_seconds: DEFAULT_MIN_SECONDS,
            occupancy_unit: OccupancyUnit::Seconds,
            notional_usd_per_hour: DEFAULT_NOTIONAL_USD_PER_HOUR.to_string(),
            settlement: SettlementHint::not_charged_here(),
        })
    }

    /// Notional USD for `seconds` of occupancy, applying the minimum. Not charged.
    pub fn notional_usd_for_seconds(&self, seconds: u64) -> String {
        let billed = seconds.max(self.min_seconds);
        let hourly: f64 = self.notional_usd_per_hour.parse().unwrap_or(0.048);
        format!("{:.6}", hourly * (billed as f64) / 3600.0)
    }
}

/// Why a quote cannot be issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QuoteError {
    /// `vcpu=0` is rejected (not unlimited).
    #[error("vcpu must be > 0 (0 is not unlimited)")]
    ZeroVcpu,
    /// `mem_gib=0` is rejected.
    #[error("mem_gib must be > 0 (0 is not unlimited)")]
    ZeroMemory,
    /// `disk_gib=0` is rejected.
    #[error("disk_gib must be > 0")]
    ZeroDisk,
}

/// Newtype lease identifier (`l_<uuid>`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LeaseId(pub String);

impl LeaseId {
    /// Allocate a new lease id.
    pub fn generate() -> Self {
        Self(format!("l_{}", Uuid::new_v4().simple()))
    }
}

impl std::fmt::Display for LeaseId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Lifecycle of a lease on this node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseState {
    /// Guest is held for the tenant.
    Live,
    /// Guest was destroyed (v1 revert) and a receipt exists.
    Ended,
}

/// Why a lease ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    /// Graceful end (`DELETE` / `berth` end). Occupancy is recorded.
    Graceful,
}

/// A computer-session lease. One live lease per node in v1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// Lease id.
    pub id: LeaseId,
    /// Current state.
    pub state: LeaseState,
    /// Occupancy quote captured at create time.
    pub quote: Quote,
    /// When the guest was created.
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    /// When the guest was destroyed, if ended.
    #[serde(with = "time::serde::rfc3339::option")]
    pub ended_at: Option<OffsetDateTime>,
    /// Loopback-only guest view (noVNC or equivalent) for this live lease.
    /// Absent after the lease ends. Never a bind-all URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_url: Option<String>,
}

/// Occupancy receipt. Seconds held, not clicks or agent actions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    /// Lease this receipt closes.
    pub lease_id: LeaseId,
    /// Wall-clock seconds the guest was held.
    pub occupancy_seconds: u64,
    /// Minimum occupancy applied by the quote.
    pub min_seconds: u64,
    /// `max(occupancy_seconds, min_seconds)`.
    pub billed_seconds: u64,
    /// Always [`OccupancyUnit::Seconds`].
    pub occupancy_unit: OccupancyUnit,
    /// Notional USD for `billed_seconds`. Not an invoice.
    pub notional_usd: String,
    /// How the lease ended.
    pub reason: EndReason,
    /// Reminder that this repo does not settle.
    pub settlement: SettlementHint,
}

impl Receipt {
    /// Close a live lease. Occupancy is wall time, never click count.
    pub fn from_lease(lease: &Lease, ended_at: OffsetDateTime, reason: EndReason) -> Self {
        let elapsed = (ended_at - lease.started_at).whole_seconds().max(0) as u64;
        let billed = elapsed.max(lease.quote.min_seconds);
        Self {
            lease_id: lease.id.clone(),
            occupancy_seconds: elapsed,
            min_seconds: lease.quote.min_seconds,
            billed_seconds: billed,
            occupancy_unit: OccupancyUnit::Seconds,
            notional_usd: lease.quote.notional_usd_for_seconds(elapsed),
            reason,
            settlement: SettlementHint::not_charged_here(),
        }
    }
}

/// Agent/operator request to hold a guest. Secrets must never appear here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateLeaseRequest {
    /// Guest OS. v1 accepts `linux` only.
    pub os: GuestOs,
    /// Optional vCPU override. Default 2. `0` is rejected.
    #[serde(default)]
    pub vcpu: Option<u32>,
    /// Optional memory override. Default 4. `0` is rejected.
    #[serde(default)]
    pub mem_gib: Option<u32>,
    /// Optional disk override. Default 40.
    #[serde(default)]
    pub disk_gib: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    #[test]
    fn occupancy_unit_is_seconds_not_clicks() {
        let quote = Quote::linux_isolated(2, 4, 40).expect("default shape");
        assert_eq!(quote.occupancy_unit, OccupancyUnit::Seconds);
        let encoded = serde_json::to_string(&quote).expect("json");
        assert!(encoded.contains("seconds"));
        assert!(!encoded.contains("click"));
    }

    #[test]
    fn quote_rejects_zero_resources() {
        assert_eq!(Quote::linux_isolated(0, 4, 40), Err(QuoteError::ZeroVcpu));
        assert_eq!(Quote::linux_isolated(2, 0, 40), Err(QuoteError::ZeroMemory));
    }

    #[test]
    fn receipt_meters_seconds_and_applies_minimum() {
        let quote = Quote::linux_isolated(2, 4, 40).unwrap();
        let started = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let lease = Lease {
            id: LeaseId("l_test".into()),
            state: LeaseState::Live,
            quote,
            started_at: started,
            ended_at: None,
            viewer_url: None,
        };
        let ended = started + Duration::seconds(12);
        let receipt = Receipt::from_lease(&lease, ended, EndReason::Graceful);
        assert_eq!(receipt.occupancy_seconds, 12);
        assert_eq!(receipt.billed_seconds, DEFAULT_MIN_SECONDS);
        assert_eq!(receipt.occupancy_unit, OccupancyUnit::Seconds);
        assert!(!receipt.settlement.charged_here);
        assert!(receipt.settlement.note.contains("berth-market"));
    }

    #[test]
    fn settlement_hint_never_claims_payment() {
        let hint = SettlementHint::not_charged_here();
        assert!(!hint.charged_here);
    }
}
