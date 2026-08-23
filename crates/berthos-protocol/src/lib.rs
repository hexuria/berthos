//! Shared types for a Berthos computer-session node.
//!
//! This crate is the socket, not the market. Quotes and receipts meter
//! **occupancy seconds**. They do not collect money, mint a token, or list
//! inventory. Listings and settlement live in the sibling repo
//! [`berth-market`](https://github.com/hexuria/berth-market).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod eligibility;
mod session;

pub use eligibility::{
    Chassis, CheckId, CheckStatus, DoctorCheck, DoctorReport, EgressPolicy, Facts, GuestImage,
    GuestOs, Intent, NodeClass, MIN_FREE_MEM_GIB, MIN_FREE_VCPU, REQUIRED_DESKTOP_LABEL,
    REQUIRED_EGRESS_POLICY, REQUIRED_GUEST_IMAGE, REQUIRED_GUEST_VERSION,
};
pub use session::{
    CreateLeaseRequest, Density, EndReason, Lease, LeaseId, LeaseState, OccupancyUnit, Quote,
    QuoteError, Receipt, SettlementHint, DEFAULT_DISK_GIB, DEFAULT_MEM_GIB, DEFAULT_MIN_SECONDS,
    DEFAULT_NOTIONAL_USD_PER_HOUR, DEFAULT_VCPU,
};

/// Wire protocol version this node speaks.
pub const PROTOCOL_VERSION: &str = "v1";
