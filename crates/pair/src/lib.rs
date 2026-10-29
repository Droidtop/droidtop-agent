//! Pairing, from the computer.
//! - `droidtop-agent pair <code>`: the handheld shows the code (Settings >
//!   Computers > Pair a computer on droidtop) and this computer connects to
//!   it, found on the LAN or at the address in the QR code's text.
//! - `droidtop-agent pair`: this computer shows its addresses and a code, and
//!   the handheld connects (Pair a computer > "Use a code from the computer").
//!   For a handheld this computer cannot reach: one behind an emulator's or a
//!   guest network's NAT.
//!
//! A crate of its own (docs/DESIGN.md section 13): it needs only this
//! computer's [`Store`], so the window, the command line and a desktop
//! manager (Pythia) pair the same way.

use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use droidtop_agent_core::accept::{Cancel, Ended, Waiting};
use droidtop_agent_core::confirm::{self, Comparing};
use droidtop_agent_core::pairing::{accept_pairing, new_code, pair_client, Kind, PairInvite, Paired, PairingEnded, MAX_ATTEMPTS};
use droidtop_agent_core::{discovery, PAIR_PORT, PORT};

use droidtop_agent_store::Store;

/// How long the computer shows a code.
const SHOW_FOR: Duration = Duration::from_secs(10 * 60);

/// This computer showing a code: the port it listens on, its addresses and
/// the code, until a handheld pairs or the time runs out.
pub struct Showing {
    listener: TcpListener,
    pub port: u16,
    pub code: String,
    /// `ip:port` for each of this computer's IPv4 addresses.
    pub addresses: Vec<String>,
    /// Every address a handheld can use, labelled (see [`places`]), with this port.
    pub places: Vec<Place>,
    pub until: Instant,
}

impl Showing {
    /// The invitation as text, with this computer's first address: what a
    /// QR code of it carries.
    pub fn invite(&self, store: &Store) -> String {
        PairInvite { code: self.code.clone(), peer: Some(store.peer_id()), name: Some(store.name()), at: self.addresses.first().cloned() }
            .to_uri()
    }
}

/// An address a handheld can reach this computer at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// What kind of network it is on, in the person's words ("Local network",
    /// "Tailscale", "ZeroTier", "VPN", "Virtual machines (WSL, Hyper-V, Docker)", "IPv6").
    pub label: String,
    /// `ip:port`, or `[ip]:port` for IPv6.
    pub address: String,
}

/// Every address of this computer a handheld can use, with [`port`], one list
/// for the pairing screens and the command line: each interface's IPv4
/// address, then global IPv6 addresses. The address the internet sees is not
/// among them (the Status page shows it last, as "from outside your network").
pub fn places(port: u16) -> Vec<Place> {
    let mut v4 = Vec::new();
    let mut v6 = Vec::new();
    for i in if_addrs::get_if_addrs().unwrap_or_default() {
        if i.is_loopback() {
            continue;
        }
        match i.ip() {
            std::net::IpAddr::V4(ip) if !ip.is_link_local() => {
                v4.push(Place { label: network_kind(&i.name, ip), address: format!("{ip}:{port}") });
            }
            std::net::IpAddr::V6(ip) if ip.segments()[0] & 0xe000 == 0x2000 => {
                v6.push(Place { label: "IPv6".into(), address: format!("[{ip}]:{port}") });
            }
            _ => {}
        }
    }
    // The ones on the person's own network first, then VPNs, then virtual machines' networks.
    v4.sort_by_key(|p| match p.label.as_str() {
        "Local network" => 0,
        l if l.starts_with("Virtual") => 3,
        "Tailscale" | "ZeroTier" | "VPN" => 1,
        _ => 2,
    });
    v4.extend(v6);
    v4
}

/// What an interface is, from its name and address.
fn network_kind(name: &str, ip: std::net::Ipv4Addr) -> String {
    let n = name.to_lowercase();
    let o = ip.octets();
    let label = if n.contains("tailscale") || (o[0] == 100 && (64..128).contains(&o[1])) {
        "Tailscale"
    } else if n.contains("zerotier") || n.starts_with("zt") {
        "ZeroTier"
    } else if n.contains("wireguard") || n.starts_with("wg") || n.contains("tun") || n.contains("vpn") || n.contains("ppp") {
        "VPN"
    } else if n.contains("vethernet")
        || n.contains("wsl")
        || n.contains("docker")
        || n.starts_with("veth")
        || n.starts_with("br-")
        || n.contains("virtual")
        || n.contains("vmnet")
        || n.contains("hyper-v")
    {
        "Virtual machines (WSL, Hyper-V, Docker)"
    } else {
        "Local network"
    };
    label.into()
}

/// Opens the pairing port and makes a new code.
pub fn listen() -> Result<Showing, String> {
    let listener = TcpListener::bind(("0.0.0.0", PAIR_PORT))
        .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
        .map_err(|e| format!("Could not open a port for pairing: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let all = places(port);
    let addresses = all.iter().filter(|p| !p.address.starts_with('[')).map(|p| p.address.clone()).collect();
    Ok(Showing { listener, port, code: new_code(), addresses, places: all, until: Instant::now() + SHOW_FOR })
}

/// Shows this computer's addresses and a new code on the console, and waits for a handheld.
pub fn show(store: &Store) -> Result<String, String> {
    let showing = listen()?;
    let (port, code) = (showing.port, &showing.code);
    println!("On droidtop: Settings > Computers > Pair a computer > Use a code from the computer.");
    if showing.places.is_empty() {
        println!("Address: this computer's address, port {port}");
    }
    for (i, place) in showing.places.iter().enumerate() {
        println!("{} {}  ({})", if i == 0 { "Address:" } else { "     or:" }, place.address, place.label);
    }
    println!("Code:    {} {}", &code[..3], &code[3..]);
    if !showing.addresses.is_empty() {
        println!("On another computer: droidtop-agent pair '{}'", showing.invite(store));
    }
    println!("(An Android emulator reaches this computer at 10.0.2.2:{port}.) Waiting up to 10 minutes; Ctrl+C stops.");
    wait(store, showing, &Cancel::new())
}

/// Waits for a handheld or another computer to pair with the code [`showing`] shows, until it
/// pairs, three codes were wrong, the time runs out or [`cancel`] is called.
pub fn wait(store: &Store, showing: Showing, cancel: &Cancel) -> Result<String, String> {
    let Showing { listener, code, until: deadline, .. } = showing;
    match accept_pairing(&listener, &store.key, &store.name(), Kind::Computer, &code, cancel, deadline, |from, e| {
        eprintln!("A pairing attempt from {from} failed: {e}")
    }) {
        Ok((paired, _)) => keep(store, paired),
        Err(PairingEnded::Cancelled) => Err("Pairing stopped.".into()),
        Err(PairingEnded::TimedOut) => Err("Nothing paired in time. Run droidtop-agent pair again for a new code.".into()),
        Err(PairingEnded::TooManyWrong(_)) => {
            Err(format!("Stopped after {MAX_ATTEMPTS} wrong codes. Run droidtop-agent pair again for a new one."))
        }
        Err(PairingEnded::Io(e)) => Err(e.to_string()),
    }
}

pub fn pair(store: &Store, input: &str) -> Result<String, String> {
    let invite =
        PairInvite::parse(input).ok_or("That is not a pairing code: type the 6 digits the handheld shows, or the text of its QR code.")?;
    let address: SocketAddr = match &invite.at {
        Some(at) => at.parse().map_err(|_| format!("The QR code's address {at} is not valid."))?,
        None => {
            let found = discovery::find_pairing(Duration::from_secs(3))
                .map_err(|e| format!("Could not look for the handheld on this network: {e}"))?;
            let wanted: Vec<_> = match &invite.peer {
                Some(peer) => found.into_iter().filter(|(a, _)| a.id == peer.to_hex()).collect(),
                None => found,
            };
            match wanted.as_slice() {
                [] => return Err("No handheld is showing a pairing code on this network. On droidtop open Settings > Computers > Pair a computer, keep it open, and try again; or type the text of its QR code here.".into()),
                [(_, addr)] => *addr,
                many => {
                    let names: Vec<String> = many.iter().map(|(a, addr)| format!("{} at {addr}", a.name)).collect();
                    return Err(format!("More than one handheld is showing a code ({}). Use the text of the QR code to pick one.", names.join(", ")));
                }
            }
        }
    };
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(10)).map_err(|e| format!("Could not reach {address}: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(60))).map_err(|e| format!("Could not set up the connection to {address}: {e}"))?;
    let paired = pair_client(&mut stream, &store.key, &store.name(), Kind::Computer, &invite.code)
        .map_err(|e| format!("Pairing did not finish: {e}. Check the code and try again."))?;
    if invite.peer.is_some_and(|p| p != paired.peer) {
        return Err("The device that answered is not the one in the invitation; nothing was paired.".into());
    }
    keep(store, paired)
}

/// Pins a device that paired, by its certificate's key: a handheld, or
/// another computer (owner, 2026-10-16: computer-to-computer pairing is a
/// Pythia feature; docs/DESIGN.md section 3, "Kinds of device").
fn keep(store: &Store, paired: Paired) -> Result<String, String> {
    store.add_device(paired.peer, &paired.name, paired.kind).map_err(|e| format!("Paired, but the pairing could not be saved: {e}"))?;
    Ok(paired.name)
}

/// How long a comparison waits for the person to answer on this screen
/// and on the other device.
const ANSWER_FOR: Duration = Duration::from_secs(120);

/// This computer offering to pair by comparing codes (droidtop SPEC 7o,
/// "Pairing with Pythia"): it answers `DTCQ1` with [`Offering::offer`] and
/// listens for the device that picks it. Dropping it stops both.
pub struct Offering {
    listener: TcpListener,
    pub offer: discovery::Offer,
    stop: Arc<AtomicBool>,
    answering: Option<std::thread::JoinHandle<()>>,
}

/// Starts offering. [`program`] is what this computer runs (`pythia` or
/// `droidtop-agent`). When nothing else in this process answers on the
/// agent's port (it is not serving), this answers there itself.
pub fn offer(store: &Store, program: &str) -> Result<Offering, String> {
    let listener = TcpListener::bind(("0.0.0.0", PAIR_PORT))
        .or_else(|_| TcpListener::bind(("0.0.0.0", 0)))
        .map_err(|e| format!("Could not open a port for pairing: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let offer = discovery::Offer { id: store.peer_id().to_hex(), name: store.name(), port, program: program.into() };
    discovery::offer(Some(offer.clone()));
    let stop = Arc::new(AtomicBool::new(false));
    let answering = UdpSocket::bind(("0.0.0.0", PORT)).ok().map(|sock| {
        let stop = stop.clone();
        std::thread::spawn(move || {
            let _ = discovery::answer_offers(&sock, &stop);
        })
    });
    Ok(Offering { listener, offer, stop, answering })
}

impl Drop for Offering {
    fn drop(&mut self) {
        discovery::offer(None);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.answering.take() {
            let _ = t.join();
        }
    }
}

/// A comparison under way: [`code`](Comparison::code) is what this screen
/// shows, [`name`](Comparison::name) who is on the other end when known.
pub struct Comparison {
    stream: TcpStream,
    comparing: Comparing,
    pub code: String,
    pub name: String,
}

impl Offering {
    /// Waits for the one device that picks this computer, until [`cancel`]
    /// is set, and starts the comparison. The offer ends here: one attempt
    /// each time the screen opens.
    pub fn accept(self, store: &Store, cancel: &Cancel) -> Result<Comparison, String> {
        let mut stream = {
            let waiting = Waiting::new(&self.listener, cancel, None).map_err(|e| e.to_string())?;
            match waiting.accept() {
                Ok((stream, _)) => stream,
                Err(Ended::Io(e)) => return Err(e.to_string()),
                Err(Ended::Cancelled | Ended::TimedOut) => return Err("Pairing stopped.".into()),
            }
        };
        stream.set_read_timeout(Some(ANSWER_FOR)).map_err(|e| e.to_string())?;
        drop(self);
        let comparing = confirm::host_start(&mut stream, &store.key).map_err(|e| format!("Pairing did not start: {e}"))?;
        Ok(Comparison::new(stream, comparing, String::new()))
    }
}

impl Comparison {
    fn new(stream: TcpStream, comparing: Comparing, name: String) -> Comparison {
        Comparison { code: comparing.code.clone(), stream, comparing, name }
    }

    /// The person's answer: [`same`] when the two screens show the same
    /// code. Pins the other device when both said so, and returns its name.
    pub fn finish(mut self, store: &Store, same: bool) -> Result<String, String> {
        let paired = self
            .comparing
            .finish(&mut self.stream, &store.key, &store.name(), Kind::Computer, same)
            .map_err(|e| format!("Nothing was paired: {e}"))?;
        keep(store, paired)
    }
}

/// Computers on this network offering to pair by comparing codes, other
/// than this one, within [`timeout`].
pub fn computers(store: &Store, timeout: Duration) -> Result<Vec<(discovery::Offer, SocketAddr)>, String> {
    let me = store.peer_id().to_hex();
    let found = discovery::find_computers(timeout).map_err(|e| format!("Could not look for computers on this network: {e}"))?;
    Ok(found.into_iter().filter(|(o, _)| o.id != me).collect())
}

/// Picks [`offer`] at [`at`] and starts the comparison with it.
pub fn pick(store: &Store, offer: &discovery::Offer, at: SocketAddr) -> Result<Comparison, String> {
    let mut stream =
        TcpStream::connect_timeout(&at, Duration::from_secs(10)).map_err(|e| format!("Could not reach {} at {at}: {e}", offer.name))?;
    stream.set_read_timeout(Some(ANSWER_FOR)).map_err(|e| format!("Could not set up the connection to {at}: {e}"))?;
    let comparing = confirm::client_start(&mut stream, &store.key).map_err(|e| format!("Pairing did not start: {e}"))?;
    if comparing.other.to_hex() != offer.id {
        return Err(format!("The device at {at} is not {}; nothing was paired.", offer.name));
    }
    Ok(Comparison::new(stream, comparing, offer.name.clone()))
}

fn ask(question: &str) -> String {
    use std::io::Write as _;
    print!("{question} ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line.trim().to_lowercase()
}

fn confirm_on_console(c: Comparison, store: &Store) -> Result<String, String> {
    println!("Code: {} {}", &c.code[..3], &c.code[3..]);
    let same = ask("Does the other screen show the same code? (y/n)").starts_with('y');
    c.finish(store, same)
}

/// `droidtop-agent pair compare`: offers to pair until a device picks this
/// computer, then compares codes on the console.
pub fn compare_offer(store: &Store) -> Result<String, String> {
    let offering = offer(store, "droidtop-agent")?;
    println!("{} can be paired: on the handheld or the other computer, pick it from the list. Ctrl+C stops.", offering.offer.name);
    let c = offering.accept(store, &Cancel::new())?;
    confirm_on_console(c, store)
}

/// `droidtop-agent pair find`: lists the computers offering to pair and
/// compares codes with the one picked.
pub fn compare_find(store: &Store) -> Result<String, String> {
    let found = computers(store, Duration::from_secs(3))?;
    let (o, at) = match found.as_slice() {
        [] => return Err("No computer is offering to pair on this network. On the other computer open Pythia's Devices page, or run droidtop-agent pair compare.".into()),
        [one] => one.clone(),
        many => {
            for (i, (o, at)) in many.iter().enumerate() {
                println!("{}. {} ({}, {at})", i + 1, o.name, o.program);
            }
            let n: usize = ask("Which one?").parse().map_err(|_| "Nothing was picked.".to_string())?;
            many.get(n.wrapping_sub(1)).cloned().ok_or("Nothing was picked.")?
        }
    };
    let c = pick(store, &o, at)?;
    confirm_on_console(c, store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn interfaces_are_named_for_what_they_are() {
        assert_eq!(network_kind("Ethernet", Ipv4Addr::new(192, 168, 1, 20)), "Local network");
        assert_eq!(network_kind("Tailscale", Ipv4Addr::new(100, 91, 90, 11)), "Tailscale");
        assert_eq!(network_kind("eth9", Ipv4Addr::new(100, 100, 1, 1)), "Tailscale");
        assert_eq!(network_kind("ZeroTier One Virtual Port", Ipv4Addr::new(10, 147, 17, 3)), "ZeroTier");
        assert_eq!(network_kind("wg0", Ipv4Addr::new(10, 8, 0, 2)), "VPN");
        assert!(network_kind("vEthernet (WSL)", Ipv4Addr::new(172, 25, 128, 1)).starts_with("Virtual"));
    }

    fn temp_store(name: &str) -> Store {
        use droidtop_agent_store::Dirs;
        let root = std::env::temp_dir().join(format!("dta-pair-{name}-{}", std::process::id()));
        let dirs = Dirs { config: root.join("config"), data: root.join("data"), computer: root.join("computer") };
        for p in [&dirs.config, &dirs.data, &dirs.computer] {
            std::fs::create_dir_all(p).unwrap();
        }
        Store::open_in(dirs).unwrap()
    }

    fn compare(a_says: bool, b_says: bool, tag: &str) -> (Result<String, String>, Result<String, String>, Store, Store) {
        let (a, b) = (temp_store(&format!("{tag}-a")), temp_store(&format!("{tag}-b")));
        let offering = offer(&a, "pythia").unwrap();
        let (o, port) = (offering.offer.clone(), offering.offer.port);
        let waiting = std::thread::spawn(move || {
            let c = offering.accept(&a, &Cancel::new()).unwrap();
            let code = c.code.clone();
            (c.finish(&a, a_says), code, a)
        });
        let c = pick(&b, &o, SocketAddr::from(([127, 0, 0, 1], port))).unwrap();
        let code = c.code.clone();
        let rb = c.finish(&b, b_says);
        let (ra, a_code, a) = waiting.join().unwrap();
        assert_eq!(a_code, code);
        (ra, rb, a, b)
    }

    /// droidtop SPEC 7o: one code on both screens, confirmed on both.
    #[test]
    fn a_computer_picked_from_the_list_pairs_by_comparing_codes() {
        let (ra, rb, a, b) = compare(true, true, "cmp");
        assert!(ra.is_ok() && rb.is_ok(), "{ra:?} {rb:?}");
        assert!(a.devices_now().iter().any(|d| d.id == b.peer_id().to_hex() && d.kind == Some(Kind::Computer)));
        assert!(b.devices_now().iter().any(|d| d.id == a.peer_id().to_hex() && d.kind == Some(Kind::Computer)));
    }

    #[test]
    fn a_no_on_either_screen_pins_nothing() {
        let (ra, rb, a, b) = compare(true, false, "no");
        assert!(ra.is_err() && rb.is_err());
        assert!(a.devices_now().is_empty() && b.devices_now().is_empty());
    }

    /// Owner, 2026-10-16: a computer pairs with another computer (Pythia's
    /// reason for being). One shows a code, the other types its invitation,
    /// and each pins the other as a computer.
    #[test]
    fn two_computers_pair_and_each_pins_the_other() {
        let (a, b) = (temp_store("a"), temp_store("b"));
        let showing = listen().unwrap();
        let invite = PairInvite {
            code: showing.code.clone(),
            peer: Some(a.peer_id()),
            name: Some(a.name()),
            at: Some(format!("127.0.0.1:{}", showing.port)),
        }
        .to_uri();
        let (a_peer, b_peer) = (a.peer_id(), b.peer_id());
        let waiting = std::thread::spawn(move || {
            let r = wait(&a, showing, &Cancel::new());
            (r, a)
        });
        pair(&b, &invite).unwrap();
        let (r, a) = waiting.join().unwrap();
        r.unwrap();
        let kind_of = |s: &Store, peer: droidtop_agent_core::keys::PeerId| {
            s.devices_now().into_iter().find(|d| d.id == peer.to_hex()).and_then(|d| d.kind)
        };
        assert_eq!(kind_of(&a, b_peer), Some(Kind::Computer));
        assert_eq!(kind_of(&b, a_peer), Some(Kind::Computer));
    }

    #[test]
    fn the_list_has_the_port_and_no_loopback() {
        for place in places(47612) {
            assert!(place.address.ends_with(":47612"), "{place:?}");
            assert!(!place.address.starts_with("127."), "{place:?}");
        }
    }
}
