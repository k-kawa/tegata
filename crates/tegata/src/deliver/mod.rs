//! Destinations: places that receive copies of secrets (GitHub Actions secrets).
//!
//! A destination can be written but not read back, so it is never the source of a value. What was
//! delivered (which version, and the destination's own update time) is recorded on the secret in
//! the store, in the `tegata-delivered` annotation.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::GithubDestination;

pub mod github;

/// The annotation on a secret that records its deliveries
pub const ANNOTATION: &str = "tegata-delivered";

/// One delivery of a secret
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivered {
    /// The version of the secret that was written
    pub version: u64,
    /// Where it was written (kept so that it can be removed after the destination leaves the
    /// configuration)
    pub github: GithubDestination,
    /// The name there
    pub name: String,
    /// The destination's update time of the secret right after tegata wrote it. A later time
    /// means someone changed it there
    pub updated_at: String,
}

/// Destination name → delivery
pub type Deliveries = BTreeMap<String, Delivered>;

pub fn deliveries(meta: &crate::store::SecretMeta) -> Deliveries {
    meta.annotations
        .get(ANNOTATION)
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_default()
}

/// A place that receives copies of secrets
#[async_trait::async_trait]
pub trait Destination: Send + Sync {
    /// The secrets there: name (as the destination reports it) → update time
    async fn list(&self) -> anyhow::Result<BTreeMap<String, String>>;
    async fn put(&self, name: &str, value: &str) -> anyhow::Result<()>;
    /// Removing a secret that does not exist is not an error
    async fn delete(&self, name: &str) -> anyhow::Result<()>;
}
