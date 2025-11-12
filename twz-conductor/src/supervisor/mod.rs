use std::{path::PathBuf, time::Duration};
use anyhow::Result;
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct ServiceCfg {
    pub image: String,
    pub name: String,
    pub env: Option<Vec<String>>,
    pub port: Option<u16>,
    pub args: Option<Vec<String>>,
}

#[derive(Deserialize, Clone)]
pub struct ConductorCfg {
    pub network: String,
    pub project: String,
    pub nats: ServiceCfg,
    pub gateway: ServiceCfg,
    pub cellservers: Vec<ServiceCfg>,
}

pub enum Backend {
    Docker,
    // K8s later
}

pub struct Supervisor {
    backend: Backend,
    cfg: ConductorCfg,
    interval: Duration,
}

impl Supervisor {
    pub fn from_toml_file(backend: Backend, path: PathBuf, interval: Duration) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cfg: ConductorCfg = toml::from_str(&text)?;
        Ok(Self { backend, cfg, interval })
    }

    pub async fn run(self) -> Result<()> {
        match self.backend {
            Backend::Docker => crate::supervisor::docker::run(self.cfg, self.interval).await,
        }
    }
}

pub(crate) mod docker;