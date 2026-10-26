//! What the gateway lets through from a player, kept apart from the network code.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// The longest client message. Real ones are a few hundred bytes; the C server reads
/// most into 256 or 4096 byte buffers.
pub const MAX_FRAME: usize = 4096;

/// Every command the C server takes from a client (`player_commands` and
/// `client_commands` in socket/loop.cpp). Anything else closes the connection.
const ALLOWED: [&str; 22] = [
    "examine", "apply", "move", "reply", "ncom", "lookat", "lock", "mark", "inscribe", "addme", "askface", "beat",
    "requestinfo", "setup", "version", "asksmooth", "accountlogin", "accountnew", "accountaddplayer", "accountplay",
    "accountpw", "createplayer",
];

/// Commands that try an account password: limited per address, so reconnecting does
/// not reset the server's own 5-tries-per-connection limit.
const LOGINS: [&str; 4] = ["accountlogin", "accountnew", "accountaddplayer", "accountpw"];

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Pass,
    /// A password try: pass it if the address has tries left.
    Login,
    Refuse(&'static str),
}

/// Checks one client message (without its 2-byte length).
pub fn check(payload: &[u8], allow_dm: bool) -> Verdict {
    if payload.is_empty() || payload.len() > MAX_FRAME {
        return Verdict::Refuse("bad length");
    }
    let word = payload.split(|&b| b == b' ').next().unwrap_or_default();
    let Some(&command) = ALLOWED.iter().find(|c| c.as_bytes() == word) else {
        return Verdict::Refuse("unknown command");
    };
    if command == "ncom" && !allow_dm && is_dm(payload.get(5..).unwrap_or_default()) {
        return Verdict::Refuse("dm over the network");
    }
    if LOGINS.contains(&command) { Verdict::Login } else { Verdict::Pass }
}

/// Whether an ncom (2-byte packet, 4-byte repeat, then the typed text) asks for DM
/// powers. The server reads the text up to a NUL, takes the first word in lower case
/// and may accept an unambiguous prefix: `dm` and `dmhide` both start with "dm".
fn is_dm(ncom: &[u8]) -> bool {
    let text = ncom.get(6..).unwrap_or_default();
    let text = text.split(|&b| b == 0).next().unwrap_or_default();
    let word = text.split(|&b| b == b' ').next().unwrap_or_default();
    word.len() >= 2 && word[..2].eq_ignore_ascii_case(b"dm")
}

/// Messages a connection may send: a burst (a new map asks for hundreds of faces) and a
/// steady rate after it.
pub struct Bucket {
    tokens: f32,
    last: Instant,
}

const BURST: f32 = 600.0;
const PER_SEC: f32 = 60.0;

impl Bucket {
    pub fn new(now: Instant) -> Self {
        Bucket { tokens: BURST, last: now }
    }

    /// Takes one message's token; false when the connection sends too fast.
    pub fn take(&mut self, now: Instant) -> bool {
        let refill = now.saturating_duration_since(self.last).as_secs_f32() * PER_SEC;
        self.tokens = (self.tokens + refill).min(BURST);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

pub const MAX_PER_ADDRESS: u32 = 4;
pub const MAX_TOTAL: u32 = 256;
const LOGIN_TRIES: usize = 10;
const LOGIN_WINDOW: Duration = Duration::from_secs(600);

/// Open connections and recent password tries per address.
#[derive(Default)]
pub struct Addresses {
    map: HashMap<IpAddr, Address>,
    total: u32,
}

#[derive(Default)]
struct Address {
    open: u32,
    logins: VecDeque<Instant>,
}

/// One IPv6 user usually has a whole /64: count it as one address.
pub fn key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6((u128::from(v6) & !0u128 << 64).into()),
        },
        v4 => v4,
    }
}

impl Addresses {
    /// Counts a new connection; false (and not counted) when there are too many.
    pub fn connect(&mut self, ip: IpAddr, now: Instant) -> bool {
        if self.map.len() > 10_000 {
            self.map.retain(|_, a| a.open > 0 || a.logins.back().is_some_and(|t| now.duration_since(*t) < LOGIN_WINDOW));
        }
        let a = self.map.entry(key(ip)).or_default();
        if a.open >= MAX_PER_ADDRESS || self.total >= MAX_TOTAL {
            return false;
        }
        a.open += 1;
        self.total += 1;
        true
    }

    pub fn disconnect(&mut self, ip: IpAddr) {
        if let Some(a) = self.map.get_mut(&key(ip)) {
            a.open = a.open.saturating_sub(1);
            self.total = self.total.saturating_sub(1);
            // Kept while it has recent tries, so a reconnect does not reset them.
            if a.open == 0 && a.logins.is_empty() {
                self.map.remove(&key(ip));
            }
        }
    }

    /// Counts a password try; false when the address has used its tries for now.
    pub fn login(&mut self, ip: IpAddr, now: Instant) -> bool {
        let a = self.map.entry(key(ip)).or_default();
        while a.logins.front().is_some_and(|t| now.duration_since(*t) >= LOGIN_WINDOW) {
            a.logins.pop_front();
        }
        if a.logins.len() >= LOGIN_TRIES {
            return false;
        }
        a.logins.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ncom(text: &str) -> Vec<u8> {
        let mut b = b"ncom ".to_vec();
        b.extend_from_slice(&[0, 1, 255, 255, 255, 255]);
        b.extend_from_slice(text.as_bytes());
        b
    }

    #[test]
    fn only_known_commands_pass() {
        assert_eq!(check(b"version 1023 1029 Crossfir3d", false), Verdict::Pass);
        assert_eq!(check(b"beat", false), Verdict::Pass);
        assert_eq!(check(b"accountlogin \x03ann\x02pw", false), Verdict::Login);
        assert_eq!(check(b"accountlogin", false), Verdict::Login);
        for bad in [&b""[..], b"hello", b"VERSION 1", b"versionx", b" version", &vec![b'a'; MAX_FRAME + 1]] {
            assert!(matches!(check(bad, false), Verdict::Refuse(_)), "{:?}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn dm_is_refused_unless_allowed() {
        for text in ["dm devdm", "DM x", "dmhide", "dmh", "Dm", "dm\0junk"] {
            assert_eq!(check(&ncom(text), false), Verdict::Refuse("dm over the network"), "{text}");
            assert_eq!(check(&ncom(text), true), Verdict::Pass, "{text}");
        }
        for text in ["north", "say dm me", " dm", "d", "", "drop all"] {
            assert_eq!(check(&ncom(text), false), Verdict::Pass, "{text}");
        }
        // Too short to read: the server discards it.
        assert_eq!(check(b"ncom \0\x01", false), Verdict::Pass);
        assert_eq!(check(b"ncom", false), Verdict::Pass);
    }

    #[test]
    fn bucket_allows_a_burst_then_the_rate() {
        let t = Instant::now();
        let mut b = Bucket::new(t);
        assert!((0..600).all(|_| b.take(t)));
        assert!(!b.take(t));
        assert!(b.take(t + Duration::from_millis(20)));
        assert!(!b.take(t + Duration::from_millis(20)));
    }

    #[test]
    fn addresses_limit_connections_and_tries() {
        let (t, ip): (Instant, IpAddr) = (Instant::now(), "203.0.113.5".parse().unwrap());
        let mut a = Addresses::default();
        assert!((0..MAX_PER_ADDRESS).all(|_| a.connect(ip, t)));
        assert!(!a.connect(ip, t));
        a.disconnect(ip);
        assert!(a.connect(ip, t));
        assert!((0..10).all(|_| a.login(ip, t)));
        assert!(!a.login(ip, t));
        // Reconnecting does not reset the tries; time does.
        (0..MAX_PER_ADDRESS).for_each(|_| a.disconnect(ip));
        assert!(a.connect(ip, t));
        assert!(!a.login(ip, t + Duration::from_secs(60)));
        assert!(a.login(ip, t + LOGIN_WINDOW));
    }

    #[test]
    fn total_is_capped() {
        let t = Instant::now();
        let mut a = Addresses::default();
        for i in 0..MAX_TOTAL {
            assert!(a.connect(IpAddr::from([10, 0, (i / 256) as u8, i as u8]), t));
        }
        assert!(!a.connect("192.0.2.1".parse().unwrap(), t));
    }

    #[test]
    fn ipv6_counts_per_64() {
        let a: IpAddr = "2001:db8:1:2:aaaa::1".parse().unwrap();
        let b: IpAddr = "2001:db8:1:2:bbbb::9".parse().unwrap();
        let c: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(key(a), key(b));
        assert_ne!(key(a), key(c));
        assert_eq!(key("::ffff:203.0.113.5".parse().unwrap()), "203.0.113.5".parse::<IpAddr>().unwrap());
    }
}
