//! Node advertisement and bind settings. No secrets belong here.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use berthos_protocol::{Chassis, EgressPolicy, GuestOs, Intent, NodeClass, REQUIRED_GUEST_IMAGE};
use serde::{Deserialize, Serialize};

/// On-disk node advertisement (`$BERTHOS_HOME/node.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeConfigFile {
    /// Advertised berth class.
    #[serde(default = "default_class")]
    pub class: NodeClass,
    /// Attested chassis.
    #[serde(default)]
    pub chassis: Chassis,
    /// Private vs public participation.
    #[serde(default)]
    pub intent: Intent,
    /// Guest OS that will be leased.
    #[serde(default)]
    pub guest_os: GuestOs,
    /// Bind address. Must be loopback to pass the doctor.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// TCP port.
    #[serde(default = "default_port")]
    pub port: u16,
    /// Guest egress policy. Default-deny only.
    #[serde(default)]
    pub egress: EgressPolicy,
    /// Operator attests always-on.
    #[serde(default)]
    pub always_on: bool,
    /// Operator attests wired uplink.
    #[serde(default)]
    pub wired: bool,
    /// Guest image reference.
    #[serde(default = "default_image")]
    pub image: String,
}

fn default_class() -> NodeClass {
    NodeClass::VmGuest
}
fn default_bind() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    7432
}
fn default_image() -> String {
    REQUIRED_GUEST_IMAGE.into()
}

impl Default for NodeConfigFile {
    fn default() -> Self {
        Self {
            class: default_class(),
            chassis: Chassis::Unknown,
            intent: Intent::Private,
            guest_os: GuestOs::Linux,
            bind: default_bind(),
            port: default_port(),
            egress: EgressPolicy::DefaultDeny,
            always_on: false,
            wired: false,
            image: default_image(),
        }
    }
}

/// Parsed, ready-to-use node config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeConfig {
    /// Advertised class.
    pub class: NodeClass,
    /// Attested chassis.
    pub chassis: Chassis,
    /// Private vs public.
    pub intent: Intent,
    /// Guest OS.
    pub guest_os: GuestOs,
    /// Bind IP.
    pub bind_ip: IpAddr,
    /// Port.
    pub port: u16,
    /// Egress policy applied to guests.
    pub egress: EgressPolicy,
    /// Always-on attestation.
    pub always_on: bool,
    /// Wired attestation.
    pub wired: bool,
    /// Guest image name.
    pub image: String,
}

impl NodeConfig {
    /// Load `$BERTHOS_HOME/node.toml` or defaults.
    pub fn load(home: &Path) -> Result<Self, ConfigError> {
        let path = home.join("node.toml");
        let file = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str(&text).map_err(|e| ConfigError::Parse {
                path: path.clone(),
                message: e.to_string(),
            })?
        } else {
            NodeConfigFile::default()
        };
        Self::from_file(file)
    }

    /// Persist current advertisement.
    pub fn save(&self, home: &Path) -> Result<(), ConfigError> {
        std::fs::create_dir_all(home)?;
        let file = NodeConfigFile {
            class: self.class,
            chassis: self.chassis,
            intent: self.intent,
            guest_os: self.guest_os,
            bind: self.bind_ip.to_string(),
            port: self.port,
            egress: self.egress,
            always_on: self.always_on,
            wired: self.wired,
            image: self.image.clone(),
        };
        let text =
            toml::to_string_pretty(&file).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        std::fs::write(home.join("node.toml"), text)?;
        Ok(())
    }

    fn from_file(file: NodeConfigFile) -> Result<Self, ConfigError> {
        let bind_ip = file
            .bind
            .parse::<IpAddr>()
            .map_err(|e| ConfigError::Bind(file.bind, e.to_string()))?;
        Ok(Self {
            class: file.class,
            chassis: file.chassis,
            intent: file.intent,
            guest_os: file.guest_os,
            bind_ip,
            port: file.port,
            egress: file.egress,
            always_on: file.always_on,
            wired: file.wired,
            image: file.image,
        })
    }
}

/// Config load/save failure.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Filesystem error.
    #[error("config io: {0}")]
    Io(#[from] std::io::Error),
    /// TOML parse error.
    #[error("parse {path}: {message}", path = path.display())]
    Parse { path: PathBuf, message: String },
    /// TOML serialize error.
    #[error("serialize: {0}")]
    Serialize(String),
    /// Bind address unparseable.
    #[error("invalid bind {0:?}: {1}")]
    Bind(String, String),
}
