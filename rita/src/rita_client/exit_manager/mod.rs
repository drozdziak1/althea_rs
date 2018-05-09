use actix::prelude::*;
use babel_monitor::Babel;

use althea_types::Identity;

use settings::{ExitClientSettings, RitaClientSettings};
use SETTING;

use rita_client::rita_loop::Tick;
use rita_client::traffic_watcher::{TrafficWatcher, Watch};

use failure::Error;

use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

/// An actor which pays the exit
#[derive(Default)]
pub struct ExitManager;

impl Actor for ExitManager {
    type Context = Context<Self>;
}

impl Supervised for ExitManager {}
impl SystemService for ExitManager {
    fn service_started(&mut self, _ctx: &mut Context<Self>) {
        info!("Exit Manager started");
    }
}

impl Handler<Tick> for ExitManager {
    type Result = Result<(), Error>;

    fn handle(&mut self, _: Tick, _ctx: &mut Context<Self>) -> Self::Result {
        if SETTING.current_exit_is_set() && SETTING.current_exit_details_is_set() {
            TrafficWatcher::from_registry().do_send(Watch(
                Identity {
                    mesh_ip: SETTING.get_current_exit().exit_ip,
                    wg_public_key: SETTING.get_current_exit_details().wg_public_key.clone(),
                    eth_address: SETTING.get_current_exit_details().eth_address,
                },
                SETTING.get_current_exit_details().exit_price,
            ));
        }
        let settings_ref = &mut SETTING.write().unwrap();
        match settings_ref.exits.len() {
            0 => {
                trace!("No exit available yet, carrying on...");
                return Ok(());
            }
            1 => if settings_ref.current_exit.clone().is_none() {
                trace!("Only one exit found, setting it up...");
                settings_ref.current_exit = Some(settings_ref.exits.iter().nth(0).unwrap().clone());
                return Ok(());
            },
            _ => {
                if settings_ref.current_exit.clone().is_none()
                {
                    let stream = TcpStream::connect::<SocketAddr>(format!(
                        "[::1]:{}",
                        settings_ref.network.babel_port
                    ).parse()?)?;
                    let chosen = find_best_exit(Babel::new(stream), &mut settings_ref.exits)?;
                    println!("Chose exit:\n{:#?}", chosen);

                    settings_ref.current_exit = Some(chosen);
                }
            }
        }
        Ok(())
    }
}

pub fn find_best_exit<T: Read + Write>(
    mut babel: Babel<T>,
    exits: &HashSet<ExitClientSettings>,
) -> Result<ExitClientSettings, Error> {
    babel.start_connection()?;
    let routes = babel.parse_routes();

    trace!(
        "find_best_exit - Evaluating {} available exit(s):\n{:#?}",
        exits.len(),
        exits
    );

    trace!("find_best_exit - Routes:\n{:#?}", routes);
    Ok(exits.iter().nth(0).unwrap().clone())
}
