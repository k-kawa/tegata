//! The tegata provider protocol.
//!
//! tegata starts a provider's executable once per operation, writes one [`Request`] to its
//! standard input, and reads one [`Response`] from its standard output. Standard error is shown to
//! the user as is (never write secret values there). The protocol is described in
//! `docs/reference/provider-protocol.md`.
//!
//! A provider implements [`Provider`] and calls [`run`] from `main`:
//!
//! ```no_run
//! use std::collections::BTreeMap;
//!
//! use tegata_plugin::{Created, Kind, Provider, Request};
//!
//! struct Example;
//!
//! #[async_trait::async_trait]
//! impl Provider for Example {
//!     fn describe(&self) -> BTreeMap<String, Kind> {
//!         BTreeMap::from([("token".to_owned(), Kind::Managed)])
//!     }
//!
//!     async fn create(&self, req: &Request) -> anyhow::Result<Created> {
//!         let name = req.config_str("name")?;
//!         // ... call the service's API with req.credential("api_key")? ...
//!         Ok(Created {
//!             outputs: BTreeMap::from([("value".to_owned(), format!("token for {name}"))]),
//!             state: serde_json::json!({ "id": "token-1" }),
//!         })
//!     }
//!
//!     async fn delete(&self, req: &Request) -> anyhow::Result<()> {
//!         let _id = req.state["id"].as_str().unwrap_or_default();
//!         // ... delete the token with this ID ...
//!         Ok(())
//!     }
//! }
//!
//! fn main() {
//!     tegata_plugin::run(Example)
//! }
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The protocol version. Raised when a change would make an old provider or an old tegata
/// misread a message
pub const PROTOCOL: u32 = 1;

/// An operation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    /// Return the types the provider supports
    Describe,
    /// Make a new value; return its outputs and the state needed to delete it later
    Create,
    /// Delete what `create` made, identified by its state
    Delete,
    /// Read current values (data types)
    Read,
}

/// What kind of type a resource type is
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// tegata creates and deletes it (`create`, `delete`)
    Managed,
    /// tegata only reads it (`read`)
    Data,
}

/// What tegata sends. Unknown fields are ignored, so new fields can be added without a new
/// protocol version
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub protocol: u32,
    pub op: Op,
    /// The resource type (`type` in the YAML). Empty for `describe`
    #[serde(default, rename = "type")]
    pub kind: String,
    /// The resource's `config`
    #[serde(default)]
    pub config: serde_json::Value,
    /// The provider's `config` (account IDs and the like)
    #[serde(default)]
    pub provider_config: serde_json::Value,
    /// Credential name → value (from environment variables, or typed by the user). Never log these
    #[serde(default)]
    pub credentials: BTreeMap<String, String>,
    /// For `delete`: the state that `create` returned for the generation being deleted
    #[serde(default)]
    pub state: serde_json::Value,
    /// For `delete`: the state of the latest generation, which is kept (null if there is none).
    /// Useful for types that hold only one value, for example to check that nothing still uses
    /// the old value. Providers may ignore it
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub current_state: serde_json::Value,
    /// The directory of the YAML file (relative paths in `config` are relative to it)
    #[serde(default)]
    pub base_dir: String,
    /// The resource's name (for messages)
    #[serde(default)]
    pub resource: String,
}

/// What the provider answers. [`run`] builds it from the [`Provider`] methods
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Response {
    /// `describe`: type → kind
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, Kind>,
    /// `describe`: types that hold only one value, so making a new one stops the previous value
    /// from working at once. tegata asks the user before creating these
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<String>,
    /// `create`, `read`: output name → value
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    /// `create`: what is needed to delete the value later (an ID, a time). tegata stores it in
    /// plain text, so never put secret values here
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub state: serde_json::Value,
    /// Why the operation failed (shown to the user; never include secret values)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The result of [`Provider::create`]
#[derive(Debug, Clone, Default)]
pub struct Created {
    pub outputs: BTreeMap<String, String>,
    pub state: serde_json::Value,
}

impl Request {
    /// A string in the resource's `config` (an error if it is missing)
    pub fn config_str(&self, key: &str) -> anyhow::Result<&str> {
        self.config
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("config.{key} is required"))
    }

    /// A string in the provider's `config` (an error if it is missing)
    pub fn provider_str(&self, key: &str) -> anyhow::Result<&str> {
        self.provider_config
            .get(key)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("provider config.{key} is required"))
    }

    /// A credential (an error if it is missing or empty)
    pub fn credential(&self, key: &str) -> anyhow::Result<&str> {
        self.credentials
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("credential {key} is required"))
    }
}

/// A provider. Implement the operations its types need: `create` and `delete` for managed
/// types, `read` for data types
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Resource type → kind
    fn describe(&self) -> BTreeMap<String, Kind>;
    /// Types that hold only one value ([`Response::replaces`])
    fn replaces(&self) -> Vec<String> {
        Vec::new()
    }
    async fn create(&self, _req: &Request) -> anyhow::Result<Created> {
        anyhow::bail!("create is not supported")
    }
    async fn delete(&self, _req: &Request) -> anyhow::Result<()> {
        anyhow::bail!("delete is not supported")
    }
    async fn read(&self, _req: &Request) -> anyhow::Result<BTreeMap<String, String>> {
        anyhow::bail!("read is not supported")
    }
}

/// Reads one request from standard input, runs it, writes the response to standard output, and
/// exits (status 1 on failure). Call it from the provider's `main`.
///
/// It checks the protocol version and that the type is one [`Provider::describe`] lists before
/// calling the provider
pub fn run<P: Provider>(provider: P) -> ! {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a tokio runtime");
    let (response, ok) = rt.block_on(async {
        use tokio::io::AsyncReadExt;
        let mut input = String::new();
        if let Err(e) = tokio::io::stdin().read_to_string(&mut input).await {
            return failure(format!("cannot read the request: {e}"));
        }
        let req: Request = match serde_json::from_str(&input) {
            Ok(r) => r,
            Err(e) => return failure(format!("invalid request: {e}")),
        };
        if req.protocol != PROTOCOL {
            return failure(format!(
                "protocol {} is not supported (this provider speaks {PROTOCOL})",
                req.protocol
            ));
        }
        if req.op != Op::Describe && !provider.describe().contains_key(&req.kind) {
            return failure(format!("unknown type: {}", req.kind));
        }
        let result = match req.op {
            Op::Describe => Ok(Response {
                types: provider.describe(),
                replaces: provider.replaces(),
                ..Default::default()
            }),
            Op::Create => provider.create(&req).await.map(|c| Response {
                outputs: c.outputs,
                state: c.state,
                ..Default::default()
            }),
            Op::Delete => provider.delete(&req).await.map(|()| Response::default()),
            Op::Read => provider.read(&req).await.map(|outputs| Response {
                outputs,
                ..Default::default()
            }),
        };
        match result {
            Ok(r) => (r, true),
            Err(e) => failure(format!("{e:#}")),
        }
    });
    println!(
        "{}",
        serde_json::to_string(&response).expect("a response serializes")
    );
    std::process::exit(if ok { 0 } else { 1 })
}

fn failure(message: String) -> (Response, bool) {
    (
        Response {
            error: Some(message),
            ..Default::default()
        },
        false,
    )
}
