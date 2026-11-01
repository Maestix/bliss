//! Names for TUN connections: the lookups are answered here.
//!
//! A packet carries an address, and the name the program looked up first is
//! gone by then, so `domain` and `geosite` rules never see a TUN connection
//! (AGENTS.md §9). Where bliss is the system's DNS server — Android's VPN
//! service names one inside the tunnel — it can answer every lookup itself
//! with an address of its own, from a range nobody routes (198.18.0.0/15,
//! RFC 2544's benchmark range), and read the name back when a connection to
//! that address arrives. That is fake-IP, as Clash and sing-box call it.
//!
//! What is answered how:
//! - `A` for a name with a dot: an address from the pool, the same one for
//!   the same name while it is in the pool.
//! - `A` for a name of the local network (no dot, `.local`, `.lan`,
//!   `.home.arpa`…): the caller resolves it for real, on the real network,
//!   since a printer's name means nothing to a server abroad.
//! - `AAAA`, `HTTPS` and `SVCB`: no records. IPv6 is not carried, and an
//!   `HTTPS` record's address hints would let a browser go around the name.
//! - Anything else (`TXT`, `SRV`, `MX`, `PTR`…): the caller forwards it
//!   through the tunnel.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Mutex;

/// The first address of the pool: 198.18.0.0, never handed out itself.
const BASE: u32 = u32::from_be_bytes([198, 18, 0, 0]);
/// How many names are kept, each with an address of its own. Past this many
/// the one handed out longest ago is forgotten: the cap the house rules ask
/// of anything that grows.
pub const POOL: u32 = 16_384;
/// The addresses names are spread over: 198.18.0.1 to 198.18.255.255, four
/// times the names kept, so that a name's own address is nearly always free.
const SPACE: u32 = 65_535;
/// How far past its own address a name looks for a free one.
const PROBES: u32 = 64;
/// How long a program may keep an answer. Short, so an address that is
/// handed to another name has long been forgotten by the program that asked
/// for it first.
const TTL: u32 = 60;
/// A query bigger than this is not a query a resolver sends.
const MAX_QUERY: usize = 512;

/// The names handed out, by address and by name.
pub struct FakeNames {
    inner: Mutex<Inner>,
}

struct Inner {
    by_name: HashMap<String, u32>,
    /// Slot `i` is the address `BASE + 1 + i`.
    by_slot: HashMap<u32, String>,
    /// Slots in the order they were handed out, for forgetting the oldest.
    order: std::collections::VecDeque<u32>,
}

impl Default for FakeNames {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeNames {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                by_name: HashMap::new(),
                by_slot: HashMap::new(),
                order: std::collections::VecDeque::new(),
            }),
        }
    }

    /// Whether an address is one of the pool's: such an address is only ever
    /// one bliss handed out.
    pub fn is_fake(ip: Ipv4Addr) -> bool {
        u32::from(ip) & 0xfffe_0000 == BASE
    }

    /// The address for a name: the one it already has, or its own.
    ///
    /// A name's own address comes from the name itself (FNV-1a), the next
    /// free one after it on a clash. Handed out in turn, as 0.2.x did, the
    /// addresses meant other names after the tunnel's process restarted —
    /// a new build installed — while a browser still held its answers: its
    /// google.com went to an address bliss no longer knew, or knew as
    /// another site's (the user's ERR_CONNECTION_CLOSED on Google, 2
    /// November). Taken from the name, the address is the same after.
    pub fn address_for(&self, name: &str) -> Ipv4Addr {
        let name = name.to_ascii_lowercase();
        let mut inner = self.inner.lock().expect("names lock");
        if let Some(slot) = inner.by_name.get(&name) {
            return Ipv4Addr::from(BASE + 1 + slot);
        }
        while inner.by_name.len() >= POOL as usize {
            let Some(oldest) = inner.order.pop_front() else {
                break;
            };
            if let Some(old) = inner.by_slot.remove(&oldest) {
                inner.by_name.remove(&old);
            }
        }
        let own = fnv1a(&name) % SPACE;
        let slot = (0..PROBES)
            .map(|step| (own + step) % SPACE)
            .find(|slot| !inner.by_slot.contains_key(slot))
            .unwrap_or(own);
        if let Some(old) = inner.by_slot.remove(&slot) {
            inner.by_name.remove(&old);
        }
        inner.by_slot.insert(slot, name.clone());
        inner.by_name.insert(name, slot);
        inner.order.push_back(slot);
        Ipv4Addr::from(BASE + 1 + slot)
    }

    /// The name an address was handed out for, while it still is.
    pub fn name_of(&self, ip: Ipv4Addr) -> Option<String> {
        let offset = u32::from(ip).checked_sub(BASE + 1)?;
        let inner = self.inner.lock().expect("names lock");
        inner.by_slot.get(&offset).cloned()
    }

    /// How many names are held now.
    pub fn len(&self) -> usize {
        self.inner.lock().expect("names lock").by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// What to do with one DNS query; `None` for something that is not one.
    pub fn answer(&self, packet: &[u8]) -> Option<Answer> {
        let query = Query::read(packet)?;
        Some(match query.qtype {
            TYPE_A if is_local(&query.name) => Answer::Local(query),
            TYPE_A => {
                let ip = self.address_for(&query.name);
                Answer::Faked {
                    reply: query.reply(&[ip]),
                    name: query.name,
                }
            }
            TYPE_AAAA | TYPE_HTTPS | TYPE_SVCB => Answer::Ready(query.reply(&[])),
            _ => Answer::Forward(query),
        })
    }
}

/// One query's fate.
#[derive(Debug)]
pub enum Answer {
    /// Answered here: send this back.
    Ready(Vec<u8>),
    /// Answered here with an address from the pool, for this name: send the
    /// reply back, and the caller may learn the name's real address ahead of
    /// the connection that will follow (`Addresses`).
    Faked { reply: Vec<u8>, name: String },
    /// A name of the local network: resolve it on the real network and
    /// answer with `Query::reply`.
    Local(Query),
    /// Send the query through the tunnel as it came, and its answer back.
    Forward(Query),
}

const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_SVCB: u16 = 64;
const TYPE_HTTPS: u16 = 65;

/// A query, as far as answering it needs.
#[derive(Debug, Clone)]
pub struct Query {
    id: u16,
    /// The recursion-desired bit and the opcode, copied into the answer.
    flags: u16,
    /// Lower case, without the final dot.
    pub name: String,
    pub qtype: u16,
    /// The question as it came: name, type and class.
    question: Vec<u8>,
}

impl Query {
    fn read(packet: &[u8]) -> Option<Self> {
        if packet.len() < 12 || packet.len() > MAX_QUERY {
            return None;
        }
        let id = u16::from_be_bytes([packet[0], packet[1]]);
        let flags = u16::from_be_bytes([packet[2], packet[3]]);
        // A response, or anything but one plain question, is not ours.
        if flags & 0x8000 != 0 || (flags >> 11) & 0xf != 0 {
            return None;
        }
        if u16::from_be_bytes([packet[4], packet[5]]) != 1 {
            return None;
        }
        let mut at = 12;
        let mut labels = Vec::new();
        loop {
            let len = *packet.get(at)? as usize;
            at += 1;
            if len == 0 {
                break;
            }
            // A pointer has no place in a question; a label is at most 63.
            if len > 63 {
                return None;
            }
            let label = packet.get(at..at + len)?;
            labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
            at += len;
        }
        let qtype = u16::from_be_bytes([*packet.get(at)?, *packet.get(at + 1)?]);
        let _class = packet.get(at + 2..at + 4)?;
        let question = packet[12..at + 4].to_vec();
        Some(Self {
            id,
            flags,
            name: labels.join("."),
            qtype,
            question,
        })
    }

    /// An answer with these addresses, or with no records at all: "the name
    /// is there, it has none of this type", which a program takes calmly.
    pub fn reply(&self, ips: &[Ipv4Addr]) -> Vec<u8> {
        self.respond(0, ips)
    }

    /// "Could not answer": what a lookup that fails on the way gets.
    pub fn failed(&self) -> Vec<u8> {
        self.respond(2, &[])
    }

    fn respond(&self, rcode: u16, ips: &[Ipv4Addr]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + self.question.len() + ips.len() * 16);
        out.extend_from_slice(&self.id.to_be_bytes());
        // QR, the query's opcode and RD, RA, and the code.
        let flags = 0x8000 | (self.flags & 0x7900) | 0x0080 | rcode;
        out.extend_from_slice(&flags.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&(ips.len() as u16).to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&self.question);
        for ip in ips {
            // The name by a pointer to the question's, at offset 12.
            out.extend_from_slice(&[0xc0, 12]);
            out.extend_from_slice(&TYPE_A.to_be_bytes());
            out.extend_from_slice(&1u16.to_be_bytes());
            out.extend_from_slice(&TTL.to_be_bytes());
            out.extend_from_slice(&4u16.to_be_bytes());
            out.extend_from_slice(&ip.octets());
        }
        out
    }
}

/// How many sites' real addresses are kept, and for how long. Past the cap
/// the oldest is forgotten first.
pub const ADDRESSES: usize = 4_096;
const ADDRESS_LIFE: std::time::Duration = std::time::Duration::from_secs(600);
/// How long a name nothing could be learnt about is left alone: long enough
/// that a page's many connections to it do not each wait on the tunnel's DNS
/// again, short enough that a hiccup does not last.
const FAILURE_LIFE: std::time::Duration = std::time::Duration::from_secs(60);

/// Sites' real addresses, learnt through the tunnel for the rules that look
/// at addresses (`geoip:`, a subnet): the pool's address says nothing about
/// a site's country. `None` remembers a name that has no IPv4 address.
#[derive(Default)]
pub struct Addresses {
    /// The address, and until when it is good.
    known: Mutex<HashMap<String, (Option<Ipv4Addr>, std::time::Instant)>>,
}

impl Addresses {
    pub fn new() -> Self {
        Self::default()
    }

    /// What is known of a name, while it is fresh: `Some(None)` for a name
    /// known to have no address, `None` for one not asked about yet.
    pub fn get(&self, name: &str) -> Option<Option<Ipv4Addr>> {
        let known = self.known.lock().expect("addresses lock");
        known
            .get(name)
            .filter(|(_, until)| std::time::Instant::now() < *until)
            .map(|(ip, _)| *ip)
    }

    pub fn put(&self, name: &str, ip: Option<Ipv4Addr>) {
        self.keep(name, ip, ADDRESS_LIFE);
    }

    /// Nothing could be learnt about the name: asked again only after a minute.
    pub fn failed(&self, name: &str) {
        self.keep(name, None, FAILURE_LIFE);
    }

    fn keep(&self, name: &str, ip: Option<Ipv4Addr>, life: std::time::Duration) {
        let now = std::time::Instant::now();
        let mut known = self.known.lock().expect("addresses lock");
        if known.len() >= ADDRESSES && !known.contains_key(name) {
            known.retain(|_, (_, until)| now < *until);
            if known.len() >= ADDRESSES
                && let Some(oldest) = known
                    .iter()
                    .min_by_key(|(_, (_, at))| *at)
                    .map(|(name, _)| name.clone())
            {
                known.remove(&oldest);
            }
        }
        known.insert(name.to_owned(), (ip, now + life));
    }

    pub fn len(&self) -> usize {
        self.known.lock().expect("addresses lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A query for a name's IPv4 address, recursion asked for.
pub fn query_a(id: u16, name: &str) -> Vec<u8> {
    let mut q = Vec::with_capacity(name.len() + 18);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.').filter(|l| !l.is_empty()) {
        let label = &label.as_bytes()[..label.len().min(63)];
        q.push(label.len() as u8);
        q.extend_from_slice(label);
    }
    q.push(0);
    q.extend_from_slice(&TYPE_A.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes());
    q
}

/// The first IPv4 address in an answer to `query_a(id, …)`; `None` for
/// another id, a failure, or an answer with none. CNAMEs before it are
/// stepped over.
pub fn first_a(answer: &[u8], id: u16) -> Option<Ipv4Addr> {
    if answer.len() < 12 || answer[..2] != id.to_be_bytes() || answer[2] & 0x80 == 0 {
        return None;
    }
    if answer[3] & 0x0f != 0 {
        return None;
    }
    let questions = u16::from_be_bytes([answer[4], answer[5]]);
    let answers = u16::from_be_bytes([answer[6], answer[7]]);
    let mut at = 12;
    // A name: labels to a zero, or ending in a pointer.
    let skip_name = |mut at: usize| -> Option<usize> {
        loop {
            let len = *answer.get(at)?;
            if len & 0xc0 == 0xc0 {
                return Some(at + 2);
            }
            at += 1 + len as usize;
            if len == 0 {
                return Some(at);
            }
        }
    };
    for _ in 0..questions {
        at = skip_name(at)? + 4;
    }
    for _ in 0..answers {
        at = skip_name(at)?;
        let kind = u16::from_be_bytes([*answer.get(at)?, *answer.get(at + 1)?]);
        let len = u16::from_be_bytes([*answer.get(at + 8)?, *answer.get(at + 9)?]) as usize;
        let data = answer.get(at + 10..at + 10 + len)?;
        if kind == TYPE_A && len == 4 {
            return Some(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        at += 10 + len;
    }
    None
}

/// FNV-1a over a name: the same number in every run, which the standard
/// hasher, keyed afresh per process, is not.
fn fnv1a(name: &str) -> u32 {
    name.bytes().fold(0x811c_9dc5, |hash, b| {
        (hash ^ u32::from(b)).wrapping_mul(0x0100_0193)
    })
}

/// A name only the local network knows.
fn is_local(name: &str) -> bool {
    !name.contains('.')
        || [
            ".local",
            ".lan",
            ".home.arpa",
            ".localdomain",
            ".internal",
            ".home",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    fn answered(names: &FakeNames, name: &str, qtype: u16) -> Vec<u8> {
        match names.answer(&query(name, qtype)) {
            Some(Answer::Ready(bytes)) | Some(Answer::Faked { reply: bytes, .. }) => bytes,
            other => panic!("not answered here: {other:?}"),
        }
    }

    /// A real answer through a CNAME: the query is read back and the A
    /// record found past the alias; another id, a failure and an answer of
    /// none give nothing.
    #[test]
    fn a_real_address_is_read_out_of_an_answer() {
        let q = query_a(0x4242, "www.example.net");
        assert_eq!(&q[..2], &[0x42, 0x42]);
        let mut a = q.clone();
        a[2] = 0x81;
        a[3] = 0x80;
        a[7] = 2; // two answers
        // www.example.net CNAME edge.example.net
        a.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0, 60, 0, 7]);
        a.extend_from_slice(&[4, b'e', b'd', b'g', b'e', 0xc0, 16]);
        // edge.example.net A 203.0.113.7
        a.extend_from_slice(&[0xc0, 33, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 203, 0, 113, 7]);
        assert_eq!(first_a(&a, 0x4242), Some(Ipv4Addr::new(203, 0, 113, 7)));
        assert_eq!(first_a(&a, 0x4243), None);
        let mut failed = a.clone();
        failed[3] = 0x83;
        assert_eq!(first_a(&failed, 0x4242), None);
        let mut none = q.clone();
        none[2] = 0x81;
        assert_eq!(first_a(&none, 0x4242), None);
        assert_eq!(first_a(&a[..20], 0x4242), None);
    }

    #[test]
    fn the_real_addresses_have_a_cap() {
        let known = Addresses::new();
        known.put("first.example", Some(Ipv4Addr::new(203, 0, 113, 1)));
        std::thread::sleep(std::time::Duration::from_millis(2));
        for i in 0..ADDRESSES {
            known.put(&format!("s{i}.example"), None);
        }
        assert_eq!(known.len(), ADDRESSES);
        assert_eq!(known.get("first.example"), None, "the oldest went first");
        assert_eq!(
            known.get(&format!("s{}.example", ADDRESSES - 1)),
            Some(None)
        );
    }

    #[test]
    fn a_name_gets_an_address_it_is_known_by() {
        let names = FakeNames::new();
        let reply = answered(&names, "Www.Example.NET", TYPE_A);
        // Same id, a response, one question, one answer.
        assert_eq!(&reply[..2], &[0x12, 0x34]);
        assert_eq!(reply[2] & 0x80, 0x80);
        assert_eq!(&reply[6..8], &[0, 1]);
        let ip = Ipv4Addr::new(
            reply[reply.len() - 4],
            reply[reply.len() - 3],
            reply[reply.len() - 2],
            reply[reply.len() - 1],
        );
        assert!(FakeNames::is_fake(ip));
        assert_eq!(names.name_of(ip).as_deref(), Some("www.example.net"));
        // Asked again, the same address.
        assert_eq!(names.address_for("www.example.net"), ip);
        assert!(!FakeNames::is_fake(Ipv4Addr::new(203, 0, 113, 7)));
    }

    #[test]
    fn the_pool_has_a_cap_and_forgets_the_oldest() {
        let names = FakeNames::new();
        let first = names.address_for("a0.example");
        for i in 1..=POOL {
            names.address_for(&format!("a{i}.example"));
        }
        assert_eq!(names.len(), POOL as usize);
        // The first name went to make room; its address may now be another's.
        assert_ne!(names.name_of(first).as_deref(), Some("a0.example"));
        names.address_for("a0.example");
        assert_eq!(names.len(), POOL as usize);
        // Every name held still reads back from its own address.
        let last = format!("a{POOL}.example");
        assert_eq!(
            names.name_of(names.address_for(&last)).as_deref(),
            Some(last.as_str())
        );
    }

    /// The tunnel's process restarts — a new build installed — while a
    /// browser keeps the answers it was given: the same name must mean the
    /// same address after, whatever was asked before it and in what order.
    #[test]
    fn a_name_keeps_its_address_across_a_restart() {
        let before = FakeNames::new();
        for site in ["a.example", "b.example", "www.google.com", "c.example"] {
            before.address_for(site);
        }
        let after = FakeNames::new();
        for site in ["c.example", "x.example", "www.google.com"] {
            after.address_for(site);
        }
        let google = before.address_for("www.google.com");
        assert_eq!(after.address_for("www.google.com"), google);
        assert_eq!(after.name_of(google).as_deref(), Some("www.google.com"));
        assert!(FakeNames::is_fake(google));
    }

    #[test]
    fn ipv6_and_https_records_are_answered_empty() {
        let names = FakeNames::new();
        for qtype in [TYPE_AAAA, TYPE_HTTPS, TYPE_SVCB] {
            let reply = answered(&names, "a.example", qtype);
            assert_eq!(&reply[6..8], &[0, 0], "type {qtype}");
            assert_eq!(reply[3] & 0x0f, 0);
        }
        assert!(names.is_empty());
    }

    #[test]
    fn local_names_and_other_types_go_elsewhere() {
        let names = FakeNames::new();
        assert!(matches!(
            names.answer(&query("printer", TYPE_A)),
            Some(Answer::Local(_))
        ));
        assert!(matches!(
            names.answer(&query("nas.home.arpa", TYPE_A)),
            Some(Answer::Local(_))
        ));
        assert!(matches!(
            names.answer(&query("a.example", 16)),
            Some(Answer::Forward(_))
        ));
        assert!(names.answer(&[0u8; 5]).is_none());
        // A response is not a query.
        let mut response = query("a.example", TYPE_A);
        response[2] |= 0x80;
        assert!(names.answer(&response).is_none());
    }

    #[test]
    fn a_local_answer_carries_the_real_addresses() {
        let Some(Answer::Local(q)) = FakeNames::new().answer(&query("printer", TYPE_A)) else {
            panic!("not local");
        };
        let reply = q.reply(&[
            Ipv4Addr::new(192, 168, 1, 20),
            Ipv4Addr::new(192, 168, 1, 21),
        ]);
        assert_eq!(&reply[6..8], &[0, 2]);
        assert_eq!(&reply[reply.len() - 4..], &[192, 168, 1, 21]);
        assert_eq!(q.failed()[3] & 0x0f, 2);
    }
}
