//! Finding a device again on the home network by its MAC address, when the
//! box gave it another address (a new lease after a power cut, a box
//! restarted, a device unplugged for a while).
//!
//! The machine's own neighbour table (`/proc/net/arp`, Linux) says where each
//! MAC was last seen. A device nobody talked to lately is not in it: one
//! byte sent to every address of the old /24 makes the system ask the
//! network who is who (ARP), and the table fills with whoever answers. Only
//! private IPv4 addresses, only the /24 of the old one; the whole network is
//! asked at most once a minute, whoever asks.
//!
//! A MAC is no proof: on a home network it is as easy to claim as an IP
//! address. The caller checks the device's own identity at the new address
//! before using it.

use std::net::Ipv4Addr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const ARP_TABLE: &str = "/proc/net/arp";
/// Long enough for a device on Wi-Fi to answer the system's question.
const SETTLE: Duration = Duration::from_millis(1500);
/// The whole network is asked at most this often.
const STIR_EVERY: Duration = Duration::from_secs(60);
/// The discard port: whatever the device makes of the byte, the system has
/// asked for its MAC first.
const DISCARD: u16 = 9;

/// When the network was last asked (one question for the whole house).
static LAST_STIR: Mutex<Option<Instant>> = Mutex::new(None);

/// `AA:BB:CC:DD:EE:FF`, `aa-bb-…`, `aabb.ccdd.eeff`, `aabbccddeeff` →
/// `aabbccddeeff`. `None` for anything else (and for the all-zero MAC of an
/// unanswered question).
#[must_use]
pub fn mac_hex(mac: &str) -> Option<String> {
    let hex = mac
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | '.'))
        .collect::<String>()
        .to_ascii_lowercase();
    (hex.len() == 12 && hex.bytes().all(|b| b.is_ascii_hexdigit()) && hex != "000000000000")
        .then_some(hex)
}

/// The resolved entries of a `/proc/net/arp` text: address and MAC
/// ([`mac_hex`]).
#[must_use]
pub fn parse_arp(text: &str) -> Vec<(Ipv4Addr, String)> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let ip = fields.next()?.parse().ok()?;
            let _hardware = fields.next()?;
            let flags = u32::from_str_radix(fields.next()?.trim_start_matches("0x"), 16).ok()?;
            let mac = mac_hex(fields.next()?)?;
            // ATF_COM: answered (a pending question shows 0x0).
            (flags & 0x2 != 0).then_some((ip, mac))
        })
        .collect()
}

/// What the neighbour table says now (empty where there is none).
pub async fn neighbours() -> Vec<(Ipv4Addr, String)> {
    tokio::fs::read_to_string(ARP_TABLE)
        .await
        .map(|text| parse_arp(&text))
        .unwrap_or_default()
}

/// The MAC last seen at `ip`, if the neighbour table knows it.
pub async fn mac_of(ip: Ipv4Addr) -> Option<String> {
    neighbours()
        .await
        .into_iter()
        .find(|(at, _)| *at == ip)
        .map(|(_, mac)| mac)
}

/// Addresses given for one MAC, at most: it answers at one, a few old
/// entries of the table may still name it.
const MAX_CANDIDATES: usize = 4;

/// Where `mac` may be in `table`, other than `old` and `skip`: addresses of
/// `old`'s /24 (a home network's, which the caller checked), at most four.
#[must_use]
pub fn pick(
    table: &[(Ipv4Addr, String)],
    mac: &str,
    old: Ipv4Addr,
    skip: &[Ipv4Addr],
) -> Vec<Ipv4Addr> {
    let Some(wanted) = mac_hex(mac) else {
        return Vec::new();
    };
    table
        .iter()
        .filter(|(ip, m)| {
            *m == wanted
                && *ip != old
                && !skip.contains(ip)
                && ip.octets()[..3] == old.octets()[..3]
        })
        .map(|(ip, _)| *ip)
        .take(MAX_CANDIDATES)
        .collect()
}

/// Whether `table` still sees `mac` at `old`: the device is there (only its
/// service is silent); looking elsewhere would find nothing.
#[must_use]
pub fn still_at(table: &[(Ipv4Addr, String)], mac: &str, old: Ipv4Addr) -> bool {
    mac_hex(mac).is_some_and(|wanted| table.iter().any(|(ip, m)| *ip == old && *m == wanted))
}

/// Where `mac` may answer now, near `old` (its last address), other than
/// `skip`: the network's /24 is asked who is who first (at most once a
/// minute for the whole house; otherwise the answers to the last question
/// are awaited). Nothing outside a private network.
pub async fn ask(mac: &str, old: Ipv4Addr, skip: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    if !old.is_private() || mac_hex(mac).is_none() {
        return Vec::new();
    }
    let fresh = |table: &[(Ipv4Addr, String)]| pick(table, mac, old, skip);
    let asked = {
        let mut last = LAST_STIR.lock().unwrap_or_else(PoisonError::into_inner);
        match *last {
            Some(at) if at.elapsed() < STIR_EVERY => Some(at),
            _ => {
                *last = Some(Instant::now());
                None
            }
        }
    };
    if let Some(at) = asked {
        // Someone asked a moment ago: wait for the answers, never ask again.
        tokio::time::sleep((SETTLE * 2).saturating_sub(at.elapsed())).await;
        return fresh(&neighbours().await);
    }
    stir(old).await;
    for _ in 0..2 {
        // A dozing Wi-Fi device answers the system's second question.
        tokio::time::sleep(SETTLE).await;
        let found = fresh(&neighbours().await);
        if !found.is_empty() {
            return found;
        }
    }
    Vec::new()
}

/// One byte to every other address of `old`'s /24: the system asks the
/// network who they are (ARP) before sending, which is all we want.
async fn stir(old: Ipv4Addr) {
    let Ok(socket) = tokio::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
        return;
    };
    let [a, b, c, _] = old.octets();
    for d in 1..=254 {
        let ip = Ipv4Addr::new(a, b, c, d);
        if ip != old {
            let _ = socket.send_to(&[0], (ip, DISCARD)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "\
IP address       HW type     Flags       HW address            Mask     Device
10.0.0.30        0x1         0x2         02:00:00:00:00:01     *        eth0
10.0.0.19        0x1         0x0         00:00:00:00:00:00     *        eth0
10.0.0.24        0x1         0x2         02:00:00:00:00:21     *        eth0
10.0.0.21        0x1         0x2         02:00:00:00:00:21     *        eth0
172.16.0.5       0x1         0x2         02:00:00:00:00:05     *        eth1
";

    #[test]
    fn macs_in_every_spelling() {
        let mac = Some("0200000000aa".to_owned());
        assert_eq!(mac_hex("02:00:00:00:00:AA"), mac);
        assert_eq!(mac_hex("02-00-00-00-00-aa"), mac);
        assert_eq!(mac_hex("0200.0000.00aa"), mac);
        assert_eq!(mac_hex("0200000000AA"), mac);
        assert_eq!(mac_hex("00:00:00:00:00:00"), None, "an unanswered question");
        assert_eq!(mac_hex("02:00:00:00:00"), None);
        assert_eq!(mac_hex("zz:00:00:00:00:aa"), None);
    }

    #[test]
    fn only_answered_entries_count() {
        let table = parse_arp(TABLE);
        assert_eq!(table.len(), 4);
        assert!(
            !table
                .iter()
                .any(|(ip, _)| *ip == Ipv4Addr::new(10, 0, 0, 19))
        );
        assert_eq!(
            table[0],
            (Ipv4Addr::new(10, 0, 0, 30), "020000000001".to_owned())
        );
    }

    #[test]
    fn the_device_is_found_elsewhere_on_its_network_only() {
        let table = parse_arp(TABLE);
        let old = Ipv4Addr::new(10, 0, 0, 21);
        assert_eq!(
            pick(&table, "02:00:00:00:00:21", old, &[]),
            [Ipv4Addr::new(10, 0, 0, 24)],
            "the old entry is not an answer"
        );
        assert!(pick(&table, "02:00:00:00:00:99", old, &[]).is_empty());
        assert!(
            pick(&table, "02:00:00:00:00:05", old, &[]).is_empty(),
            "another network is not looked at"
        );
        assert!(pick(&table, "not a mac", old, &[]).is_empty());
        // Old entries naming it too: a few candidates, never the whole table,
        // and the next ones once the first were tried.
        let crowded: Vec<_> = (40..60)
            .map(|d| (Ipv4Addr::new(10, 0, 0, d), "020000000021".to_owned()))
            .collect();
        let first = pick(&crowded, "02:00:00:00:00:21", old, &[]);
        assert_eq!(first.len(), 4);
        let next = pick(&crowded, "02:00:00:00:00:21", old, &first);
        assert_eq!(next.len(), 4);
        assert!(next.iter().all(|ip| !first.contains(ip)));
    }

    #[test]
    fn a_device_still_seen_at_its_address_has_not_moved() {
        let table = parse_arp(TABLE);
        let old = Ipv4Addr::new(10, 0, 0, 21);
        assert!(still_at(&table, "02:00:00:00:00:21", old));
        assert!(!still_at(
            &table,
            "02:00:00:00:00:21",
            Ipv4Addr::new(10, 0, 0, 30)
        ));
        assert!(
            !still_at(&table, "02:00:00:00:00:21", Ipv4Addr::new(10, 0, 0, 19)),
            "an unanswered question proves nothing"
        );
    }

    #[tokio::test]
    async fn nothing_is_asked_outside_a_home_network() {
        let old = Ipv4Addr::new(8, 8, 8, 8);
        assert!(ask("02:00:00:00:00:21", old, &[]).await.is_empty());
        assert!(
            ask("nope", Ipv4Addr::new(10, 0, 0, 21), &[])
                .await
                .is_empty()
        );
    }
}
