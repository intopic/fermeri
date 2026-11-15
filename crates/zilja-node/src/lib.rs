//! # zilja-node
//!
//! Gjithçka që bën Zilja pa varur nga validatori:
//!
//! - [`tracker`]: ndjek bankat, slotet dhe fork-et, dhe i jep pemës vetëm atë që u rrënjos.
//! - [`service`]: thread-i punëtor që përditëson pemën, ruan root-et dhe mban metrikat.
//! - [`rpc`]: serveri HTTP me `getAccountProof`, `getZiljaRoot`, `/metrics`.
//!
//! Plugin-i i Geyser (`zilja-plugin`) vetëm i kthen thirrjet e validatorit në [`tracker::Event`].

pub mod attester;
pub mod rpc;
pub mod service;
pub mod tracker;

pub use attester::{AttestOptions, AttesterInfo};
pub use service::{Metrics, Options, Service, Shared, State};
pub use tracker::{Event, Status};
