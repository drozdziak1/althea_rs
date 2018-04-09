extern crate althea_types;
extern crate config;
extern crate eui48;
extern crate num256;
extern crate toml;

extern crate failure;

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate log;

extern crate serde;
extern crate serde_json;

extern crate althea_kernel_interface;

use std::net::IpAddr;
use std::path::Path;
use std::fs::File;
use std::io::Write;
use std::thread;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::collections::HashSet;
use std::clone;

use config::Config;

use althea_types::{EthAddress, ExitRegistrationDetails, Identity};

use num256::Int256;

use althea_kernel_interface::KernelInterface;

use failure::Error;

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct NetworkSettings {
    pub own_ip: IpAddr,
    pub bounty_ip: IpAddr,
    pub babel_port: u16,
    pub rita_hello_port: u16,
    pub rita_dashboard_port: u16,
    pub bounty_port: u16,
    pub wg_private_key: String,
    pub wg_private_key_path: String,
    pub wg_public_key: String,
    pub wg_start_port: u16,
    pub peer_interfaces: HashSet<String>,
    pub manual_peers: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub external_nic: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct PaymentSettings {
    pub pay_threshold: Int256,
    pub close_threshold: Int256,
    pub close_fraction: Int256,
    pub buffer_period: u32,
    pub eth_address: EthAddress,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct ExitClientSettings {
    pub exit_ip: IpAddr,
    pub exit_registration_port: u16,
    pub wg_listen_port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<ExitClientDetails>,
    pub reg_details: ExitRegistrationDetails,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct ExitClientDetails {
    pub own_internal_ip: IpAddr,
    pub server_internal_ip: IpAddr,
    pub netmask: IpAddr,
    pub eth_address: EthAddress,
    pub wg_public_key: String,
    pub wg_exit_port: u16,
    pub exit_price: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct RitaSettings {
    pub payment: PaymentSettings,
    pub network: NetworkSettings,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_client: Option<ExitClientSettings>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct ExitNetworkSettings {
    pub exit_hello_port: u16,
    pub wg_tunnel_port: u16,
    pub exit_price: u64,
    pub own_internal_ip: IpAddr,
    pub netmask: IpAddr,
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct RitaExitSettings {
    pub db_file: String,
    pub payment: PaymentSettings,
    pub network: NetworkSettings,
    pub exit_network: ExitNetworkSettings,
}

pub trait FileWrite {
    fn write(&self, file_name: &str) -> Result<(), Error>;
}

fn spawn_watch_thread<'de, T: 'static>(
    settings: Arc<RwLock<T>>,
    mut config: Config,
    file_path: &str,
) -> Result<(), Error>
where
    T: serde::Deserialize<'de> + Sync + Send + std::fmt::Debug + Clone + Eq + FileWrite,
{
    let file_path = file_path.to_string();

    thread::spawn(move || {
        info!("Watching file {} for activity...", file_path);

        let old_settings = settings.read().unwrap().clone();

        loop {
            info!("refreshing configuration ...");
            thread::sleep(Duration::from_secs(5));

            let new_settings = settings.read().unwrap().clone();

            if old_settings == new_settings {
                // settings struct was not mutated locally
                let config = config.refresh().unwrap();
                let new_settings: T = config.clone().try_into().unwrap();
                trace!("new config: {:#?}", new_settings);
                *settings.write().unwrap() = new_settings;
            } else {
                settings.read().unwrap().write(&file_path);
            }
        }
    });

    Ok(())
}

impl RitaSettings {
    pub fn new(file_name: &str) -> Result<Self, Error> {
        let mut s = Config::new();
        s.merge(config::File::with_name(file_name).required(false))?;
        let settings: Self = s.try_into()?;

        Ok(settings)
    }

    pub fn new_watched(file_name: &str) -> Result<Arc<RwLock<Self>>, Error> {
        let mut s = Config::new();
        s.merge(config::File::with_name(file_name).required(false))?;
        let settings: Self = s.clone().try_into()?;

        let settings = Arc::new(RwLock::new(settings));

        spawn_watch_thread(settings.clone(), s, file_name).unwrap();

        Ok(settings)
    }

    pub fn get_identity(&self) -> Identity {
        let ki = KernelInterface {};
        Identity::new(
            self.network.own_ip.clone(),
            self.payment.eth_address.clone(),
            ki.get_wg_pubkey(Path::new(&self.network.wg_private_key_path))
                .unwrap(),
        )
    }

    pub fn get_exit_id(&self) -> Option<Identity> {
        let details = self.exit_client.clone()?.details?;

        Some(Identity::new(
            self.exit_client.clone()?.exit_ip,
            details.eth_address.clone(),
            details.wg_public_key.clone(),
        ))
    }
}

impl FileWrite for RitaSettings {
    fn write(&self, file_name: &str) -> Result<(), Error> {
        let ser = toml::to_string(&self).unwrap();
        let mut file = File::create(file_name)?;
        file.write_all(ser.as_bytes())?;
        file.flush().unwrap();
        Ok(())
    }
}

impl RitaExitSettings {
    pub fn new(file_name: &str) -> Result<Self, Error> {
        let mut s = Config::new();
        s.merge(config::File::with_name(file_name).required(false))?;
        let settings: Self = s.try_into()?;

        let mut file = File::create(&Path::new(&settings.network.wg_private_key_path))?;
        file.write_all(&settings.network.wg_private_key.as_bytes())?;

        Ok(settings)
    }

    pub fn new_watched(file_name: &str) -> Result<Arc<RwLock<Self>>, Error> {
        let mut s = Config::new();
        s.merge(config::File::with_name(file_name).required(false))?;
        let settings: Self = s.clone().try_into()?;

        let mut file = File::create(&Path::new(&settings.network.wg_private_key_path))?;
        file.write_all(&settings.network.wg_private_key.as_bytes())?;

        let settings = Arc::new(RwLock::new(settings));

        spawn_watch_thread(settings.clone(), s, file_name).unwrap();

        Ok(settings)
    }

    pub fn get_identity(&self) -> Identity {
        let ki = KernelInterface {};

        Identity::new(
            self.network.own_ip.clone(),
            self.payment.eth_address.clone(),
            ki.get_wg_pubkey(Path::new(&self.network.wg_private_key_path))
                .unwrap(),
        )
    }
}

impl FileWrite for RitaExitSettings {
    fn write(&self, file_name: &str) -> Result<(), Error> {
        let ser = toml::to_string(&self).unwrap();
        let mut file = File::create(file_name)?;
        file.write_all(ser.as_bytes())?;
        file.flush().unwrap();
        Ok(())
    }
}
