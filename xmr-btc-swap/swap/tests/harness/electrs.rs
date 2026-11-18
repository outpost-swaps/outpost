use crate::harness::bitcoind;
use bitcoin::Network;
use std::collections::HashMap;
use testcontainers::core::{Container, Docker, WaitForMessage};
use testcontainers::Image;

pub const HTTP_PORT: u16 = 60401;
pub const RPC_PORT: u16 = 50001;

#[derive(Debug)]
pub struct Electrs {
    tag: String,
    args: ElectrsArgs,
    entrypoint: Option<String>,
    wait_for_message: String,
    volume: String,
}

impl Image for Electrs {
    type Args = ElectrsArgs;
    type EnvVars = HashMap<String, String>;
    type Volumes = HashMap<String, String>;
    type EntryPoint = str;

    fn descriptor(&self) -> String {
        format!("mempool/electrs:{}", self.tag)
    }

    fn wait_until_ready<D: Docker>(&self, _container: &Container<'_, D, Self>) {
        // Port publication is sufficient; the wallet retries the connection.
    }

    fn args(&self) -> <Self as Image>::Args {
        self.args.clone()
    }

    fn volumes(&self) -> Self::Volumes {
        let mut volumes = HashMap::new();
        volumes.insert(self.volume.clone(), bitcoind::DATADIR.to_string());
        volumes
    }

    fn env_vars(&self) -> Self::EnvVars {
        HashMap::new()
    }

    fn with_args(self, args: <Self as Image>::Args) -> Self {
        Electrs { args, ..self }
    }

    fn with_entrypoint(self, entrypoint: &Self::EntryPoint) -> Self {
        Self {
            entrypoint: Some(entrypoint.to_string()),
            ..self
        }
    }

    fn entrypoint(&self) -> Option<String> {
        self.entrypoint.to_owned()
    }
}

impl Default for Electrs {
    fn default() -> Self {
        Electrs {
            tag: "latest".into(),
            args: ElectrsArgs::default(),
            entrypoint: Some("/bin/electrs".into()),
            wait_for_message: String::new(),
            volume: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl Electrs {
    pub fn with_tag(self, tag_str: &str) -> Self {
        Electrs {
            tag: tag_str.to_string(),
            ..self
        }
    }

    pub fn with_volume(mut self, volume: String) -> Self {
        self.volume = volume;
        self
    }

    pub fn with_daemon_rpc_addr(mut self, name: String) -> Self {
        self.args.daemon_rpc_addr = name;
        self
    }

}

#[derive(Debug, Clone)]
pub struct ElectrsArgs {
    pub network: Network,
    pub daemon_dir: String,
    pub daemon_rpc_addr: String,
    pub cookie: String,
    pub db_dir: String,
    pub electrum_rpc_addr: String,
}

impl Default for ElectrsArgs {
    fn default() -> Self {
        ElectrsArgs {
            network: Network::Regtest,
            daemon_dir: bitcoind::DATADIR.to_string(),
            daemon_rpc_addr: format!("0.0.0.0:{}", bitcoind::RPC_PORT),
            cookie: format!("{}:{}", bitcoind::RPC_USER, bitcoind::RPC_PASSWORD),
            // The electrs image runs with a different UID than bitcoind's
            // volume; keep its disposable index outside the shared datadir.
            db_dir: "/tmp/electrs-db".to_string(),
            electrum_rpc_addr: format!("0.0.0.0:{}", RPC_PORT),
        }
    }
}

impl IntoIterator for ElectrsArgs {
    type Item = String;
    type IntoIter = ::std::vec::IntoIter<String>;

    fn into_iter(self) -> <Self as IntoIterator>::IntoIter {
        let mut args = Vec::new();

        match self.network {
            Network::Testnet => args.push("--network=testnet".to_string()),
            Network::Regtest => args.push("--network=regtest".to_string()),
            Network::Bitcoin => {}
            Network::Signet => panic!("signet not yet supported"),
            Network::Testnet4 => panic!("testnet4 not yet supported"),
        }

        args.push("-vvvvv".to_string());
        args.push("--jsonrpc-import".to_string());
        args.push(format!("--db-dir={}", self.db_dir));
        args.push(format!("--daemon-dir={}", self.daemon_dir));
        args.push(format!("--cookie={}", self.cookie));
        args.push(format!("--daemon-rpc-addr={}", self.daemon_rpc_addr));
        args.push(format!("--electrum-rpc-addr={}", self.electrum_rpc_addr));

        args.into_iter()
    }
}
