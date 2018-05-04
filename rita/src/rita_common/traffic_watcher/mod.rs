use actix::prelude::*;

use althea_kernel_interface::FilterTarget;
use KI;

use althea_types::LocalIdentity;

use babel_monitor::Babel;

use rita_common::debt_keeper;
use rita_common::debt_keeper::DebtKeeper;

use num256::Int256;

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};

use ip_network::IpNetwork;

use settings::RitaCommonSettings;
use SETTING;

use failure::Error;

pub struct TrafficWatcher;

impl Actor for TrafficWatcher {
    type Context = Context<Self>;
}

impl Supervised for TrafficWatcher {}

impl SystemService for TrafficWatcher {
    fn service_started(&mut self, _ctx: &mut Context<Self>) {
        KI.init_counter(&FilterTarget::Input).unwrap();
        KI.init_counter(&FilterTarget::Output).unwrap();
        KI.init_counter(&FilterTarget::ForwardInput).unwrap();
        KI.init_counter(&FilterTarget::ForwardOutput).unwrap();

        info!("Traffic Watcher started");
    }
}

impl Default for TrafficWatcher {
    fn default() -> TrafficWatcher {
        TrafficWatcher {}
    }
}

pub struct Watch(pub Vec<(LocalIdentity, String)>);

impl Message for Watch {
    type Result = Result<(), Error>;
}

impl Handler<Watch> for TrafficWatcher {
    type Result = Result<(), Error>;

    fn handle(&mut self, msg: Watch, _: &mut Context<Self>) -> Self::Result {
        let stream = TcpStream::connect::<SocketAddr>(format!(
            "[::1]:{}",
            SETTING.get_network().babel_port
        ).parse()?)?;

        watch(Babel::new(stream), &msg.0)
    }
}

/// This traffic watcher watches how much traffic each neighbor sends to each destination
/// between the last time watch was run, (This does _not_ block the thread)
/// It also gathers the price to each destination from Babel and uses this information
/// to calculate how much each neighbor owes. It returns a list of how much each neighbor owes.
///
/// This first time this is run, it will create the rules and then immediately read and zero them.
/// (should return 0)
pub fn watch<T: Read + Write>(
    mut babel: Babel<T>,
    neighbors: &[(LocalIdentity, String)],
) -> Result<(), Error> {
    babel.start_connection()?;

    trace!("Getting routes");
    let routes = babel.parse_routes()?;
    info!("Got routes: {:?}", routes);

    let mut identities: HashMap<IpAddr, LocalIdentity> = HashMap::new();
    for ident in neighbors {
        identities.insert(ident.0.global.mesh_ip, ident.0.clone());
    }

    let mut if_to_ip: HashMap<String, IpAddr> = HashMap::new();
    for ident in neighbors {
        if_to_ip.insert(ident.clone().1, ident.0.global.mesh_ip);
    }

    let mut ip_to_if: HashMap<IpAddr, String> = HashMap::new();
    for ident in neighbors {
        ip_to_if.insert(ident.0.global.mesh_ip, ident.clone().1);
    }

    let mut destinations = HashMap::new();
    let local_price = babel.local_fee().unwrap();

    for route in &routes {
        // Only ip6
        if let IpNetwork::V6(ref ip) = route.prefix {
            // Only host addresses and installed routes
            if ip.get_netmask() == 128 && route.installed {
                destinations.insert(
                    IpAddr::V6(ip.get_network_address()),
                    Int256::from(route.price + local_price),
                );
            }
        }
    }

    destinations.insert(SETTING.get_network().own_ip, Int256::from(0));

    trace!("Getting input counters");
    let input_counters = KI.read_counters(&FilterTarget::Input)?;
    info!("Got output counters: {:?}", input_counters);

    trace!("Getting destination counters");
    let output_counters = KI.read_counters(&FilterTarget::Output)?;
    info!("Got destination counters: {:?}", output_counters);

    trace!("Getting fwd counters");
    let fwd_input_counters = KI.read_counters(&FilterTarget::ForwardInput)?;
    let fwd_output_counters = KI.read_counters(&FilterTarget::ForwardOutput)?;

    info!(
        "Got fwd counters: {:?}",
        (&fwd_input_counters, &fwd_output_counters)
    );

    let mut total_input_counters = HashMap::new();
    let mut total_output_counters = HashMap::new();

    for (k, v) in input_counters {
        *total_input_counters.entry(k).or_insert(0) += v
    }

    for (k, v) in fwd_input_counters {
        *total_input_counters.entry(k).or_insert(0) += v
    }

    for (k, v) in output_counters {
        *total_output_counters.entry(k).or_insert(0) += v
    }

    for (k, v) in fwd_output_counters {
        *total_output_counters.entry(k).or_insert(0) += v
    }

    info!("Got final input counters: {:?}", total_input_counters);
    info!("Got final output counters: {:?}", total_output_counters);

    // Flow counters should debit your neighbor which you received the packet from
    // Destination counters should credit your neighbor which you sent the packet to

    let mut debts = HashMap::new();

    // Setup the debts table
    for (_, ident) in identities.clone() {
        debts.insert(ident, Int256::from(0));
    }

    for ((ip, interface), bytes) in total_input_counters {
        if destinations.contains_key(&ip) && if_to_ip.contains_key(&interface)
            && identities.contains_key(&if_to_ip[&interface])
        {
            let id = identities[&if_to_ip[&interface]].clone();
            *debts.get_mut(&id).unwrap() -= (destinations[&ip].clone()) * bytes;
        } else {
            warn!("flow destination not found {}, {}", ip, bytes);
        }
    }

    trace!("Collated flow debts: {:?}", debts);

    for ((ip, interface), bytes) in total_output_counters {
        if destinations.contains_key(&ip) && if_to_ip.contains_key(&interface)
            && identities.contains_key(&if_to_ip[&interface])
        {
            let id = identities[&if_to_ip[&interface]].clone();
            *debts.get_mut(&id).unwrap() += (destinations[&ip].clone() - local_price) * bytes;
        } else {
            warn!("destination not found {}, {}", ip, bytes);
        }
    }

    trace!("Collated total debts: {:?}", debts);

    for (k, v) in &debts {
        trace!("collated debt for {} is {}", k.global.mesh_ip, v);
    }

    for (from, amount) in debts {
        let update = debt_keeper::TrafficUpdate {
            from: from.global.clone(),
            amount,
        };

        DebtKeeper::from_registry().do_send(update);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate mockstream;
    extern crate env_logger;

    use super::*;

    use self::mockstream::SharedMockStream;

    use std::os::unix::process::ExitStatusExt;
    use std::process::{ExitStatus, Output};

    static TABLE: &'static str =
"local fee 1024\n\
add interface wlan0 up true ipv6 fe80::1a8b:ec1:8542:1bd8 ipv4 10.28.119.131\n\
add interface wg0 up true ipv6 fe80::2cee:2fff:7380:8354 ipv4 10.0.236.201\n\
add neighbour 14f19a8 address fe80::2cee:2fff:648:8796 if wg0 reach ffff rxcost 256 txcost 256 rtt \
26.723 rttcost 912 cost 1168\n\
add neighbour 14f0640 address fe80::e841:e384:491e:8eb9 if wlan0 reach 9ff7 rxcost 512 txcost 256 \
rtt 19.323 rttcost 508 cost 1020\n\
add neighbour 14f05f0 address fe80::e9d0:498f:6c61:be29 if wlan0 reach feff rxcost 258 txcost 341 \
rtt 18.674 rttcost 473 cost 817\n\
add neighbour 14f0488 address fe80::e914:2335:a76:bda3 if wlan0 reach feff rxcost 258 txcost 256 \
rtt 22.805 rttcost 698 cost 956\n\
add xroute 10.28.119.131/32-::/0 prefix 10.28.119.131/32 from ::/0 metric 0\n\
add route 14f0820 prefix 10.28.7.7/32 from 0.0.0.0/0 installed yes id ba:27:eb:ff:fe:5b:fe:c7\
metric 1596 price 3072 fee 3072 refmetric 638 via fe80::e914:2335:a76:bda3 if wlan0\n\
add route 14f07a0 prefix 10.28.7.7/32 from 0.0.0.0/0 installed no id ba:27:eb:ff:fe:5b:fe:c7\
metric 1569 price 5032 fee 5032 refmetric 752 via fe80::e9d0:498f:6c61:be29 if wlan0\n\
add route 14f06d8 prefix 10.28.20.151/32 from 0.0.0.0/0 installed yes id ba:27:eb:ff:fe:c1:2d:d5\
metric 817 price 4008 fee 4008 refmetric 0 via fe80::e9d0:498f:6c61:be29 if wlan0\n\
add route 14f0548 prefix 10.28.244.138/32 from 0.0.0.0/0 installed yes id ba:27:eb:ff:fe:d1:3e:ba\
metric 958 price 2048 fee 2048 refmetric 0 via fe80::e914:2335:a76:bda3 if wlan0\n\
ok\n";

    static PREAMBLE: &'static str =
        "ALTHEA 0.1\nversion babeld-1.8.0-24-g6335378\nhost raspberrypi\nmy-id \
         ba:27:eb:ff:fe:09:06:dd\nok\n";

    static XROUTE_LINE: &'static str =
        "add xroute 10.28.119.131/32-::/0 prefix 10.28.119.131/32 from ::/0 metric 0";

    static ROUTE_LINE: &'static str =
        "add route 14f06d8 prefix 10.28.20.151/32 from 0.0.0.0/0 installed yes id \
         ba:27:eb:ff:fe:c1:2d:d5 metric 1306 price 4008 refmetric 0 via \
         fe80::e9d0:498f:6c61:be29 if wlan0";

    static NEIGH_LINE: &'static str =
        "add neighbour 14f05f0 address fe80::e9d0:498f:6c61:be29 if wlan0 reach ffff rxcost \
         256 txcost 256 rtt 29.264 rttcost 1050 cost 1306";

    static IFACE_LINE: &'static str =
        "add interface wlan0 up true ipv6 fe80::1a8b:ec1:8542:1bd8 ipv4 10.28.119.131";

    static PRICE_LINE: &'static str = "local price 1024";

    #[test]
    fn test_tw_calls() {
        // Mock babel_monitor
        let mut bm_stream = SharedMockStream::new();

        bm_stream.push_bytes_to_read(PREAMBLE.as_bytes());
        bm_stream.push_bytes_to_read(TABLE.as_bytes());

        let mut counter = 0;
        KI.set_mock(Box::new(move |program, args| {
            println!("Calling: {} {:?}", program, args);
            Ok(Output {
                stdout: b"".to_vec(),
                stderr: b"".to_vec(),
                status: ExitStatus::from_raw(0),
            })
        }));

        watch(Babel::new(bm_stream), &[]).unwrap();
    }

    #[test]
    fn test_on_arbitrary_socket() {
        env_logger::init();
        let mut bm_stream =
            TcpStream::connect::<SocketAddr>("[::1]:9001".parse().unwrap()).unwrap();
        watch(Babel::new(bm_stream), &[]).unwrap();
    }
}
