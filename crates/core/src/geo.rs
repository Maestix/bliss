//! Reading Xray's `geosite.dat` and `geoip.dat`.
//!
//! These two files are the reason routing is worth having: between them they
//! carry every category anyone has bothered to curate — advertising networks,
//! per-country site lists, per-country address blocks — and they ship in the
//! same archive as the engine, so we already have them on disk.
//!
//! They are protobuf, and the decoder here is about a hundred lines rather than
//! a dependency and a build step. Only three wire types occur in these files
//! and the three messages are small:
//!
//! ```text
//! GeoSiteList { repeated GeoSite entry = 1 }
//! GeoSite     { string country_code = 1; repeated Domain domain = 2 }
//! Domain      { Type type = 1; string value = 2; repeated Attribute attr = 3 }
//! Type        { Plain = 0 (substring); Regex = 1; Domain = 2 (suffix); Full = 3 }
//!
//! GeoIPList   { repeated GeoIP entry = 1 }
//! GeoIP       { string country_code = 1; repeated CIDR cidr = 2 }
//! CIDR        { bytes ip = 1; uint32 prefix = 2 }
//! ```
//!
//! **Only the categories asked for are kept.** `geoip.dat` is 19 MB and
//! `geosite.dat` is 10 MB; decoding either one whole would put tens of
//! megabytes of strings in a process whose entire justification is not doing
//! that. So the loader walks the file, reads each entry's name, and skips the
//! body of every entry nobody asked for.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

/// One geosite category, in the three shapes it matches by.
///
/// Split at load time rather than at match time: a category can hold a hundred
/// thousand entries, and the difference between a hash lookup and a scan of all
/// of them is the difference between routing being free and being felt.
#[derive(Debug, Default, Clone)]
pub struct SiteSet {
    /// Exact names.
    pub full: HashSet<String>,
    /// A name and everything under it: `example.com` matches `a.example.com`.
    pub suffix: HashSet<String>,
    /// Substrings. Few enough in practice to scan.
    pub keyword: Vec<String>,
    /// Regular expressions, counted and dropped. See `skipped_regex`.
    pub skipped_regex: usize,
}

impl SiteSet {
    pub fn len(&self) -> usize {
        self.full.len() + self.suffix.len() + self.keyword.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Does this category cover `host`?
    pub fn matches(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if self.full.contains(&host) {
            return true;
        }
        // Walk the name from the left: `a.b.example.com` offers `a.b.example.com`,
        // `b.example.com`, `example.com`, `com`. Every suffix is one hash lookup,
        // so the cost is the number of labels rather than the size of the set.
        let mut rest = host.as_str();
        loop {
            if self.suffix.contains(rest) {
                return true;
            }
            match rest.split_once('.') {
                Some((_, tail)) => rest = tail,
                None => break,
            }
        }
        self.keyword.iter().any(|k| host.contains(k.as_str()))
    }
}

/// One geoip category: the address blocks belonging to it.
#[derive(Debug, Default, Clone)]
pub struct IpSet {
    /// IPv4 blocks bucketed by first octet, so a lookup scans a few dozen
    /// entries rather than the ten thousand a country list can hold.
    v4: Vec<Vec<(u32, u8)>>,
    v6: Vec<(u128, u8)>,
    count: usize,
}

impl IpSet {
    fn new() -> Self {
        Self {
            v4: vec![Vec::new(); 256],
            v6: Vec::new(),
            count: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn insert(&mut self, ip: IpAddr, prefix: u8) {
        match ip {
            IpAddr::V4(v4) => {
                let bits = u32::from(v4);
                // A /0 would have to live in every bucket; none of these files
                // contain one, and treating it as a single bucket would make it
                // silently match almost nothing.
                if prefix == 0 {
                    for bucket in self.v4.iter_mut() {
                        bucket.push((0, 0));
                    }
                } else {
                    self.v4[(bits >> 24) as usize].push((bits, prefix.min(32)));
                }
            }
            IpAddr::V6(v6) => self.v6.push((u128::from(v6), prefix.min(128))),
        }
        self.count += 1;
    }

    pub fn matches(&self, ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(v4) => {
                let bits = u32::from(v4);
                self.v4[(bits >> 24) as usize]
                    .iter()
                    .any(|&(net, prefix)| within_v4(bits, net, prefix))
            }
            IpAddr::V6(v6) => {
                let bits = u128::from(v6);
                self.v6
                    .iter()
                    .any(|&(net, prefix)| within_v6(bits, net, prefix))
            }
        }
    }
}

fn within_v4(ip: u32, net: u32, prefix: u8) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask = u32::MAX << (32 - prefix as u32);
    ip & mask == net & mask
}

fn within_v6(ip: u128, net: u128, prefix: u8) -> bool {
    if prefix == 0 {
        return true;
    }
    let mask = u128::MAX << (128 - prefix as u32);
    ip & mask == net & mask
}

// --- loading --------------------------------------------------------------

/// Load the named categories from `geosite.dat`.
///
/// Names are matched case-insensitively, so `category-ads-all` and
/// `CATEGORY-ADS-ALL` are the same thing. A name that is not in the file comes
/// back missing from the map rather than as an error — the caller knows which
/// ones it asked for and can say which were not found, which is a better
/// message than one failure for the whole load.
pub fn load_sites(path: &Path, wanted: &[String]) -> std::io::Result<HashMap<String, SiteSet>> {
    let mut found = HashMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    let wanted: HashSet<String> = wanted.iter().map(|w| w.to_ascii_uppercase()).collect();

    for (code, entry) in wanted_entries(path, &wanted)? {
        let mut set = SiteSet::default();
        let mut reader = Reader::new(&entry);
        while let Some((number, value)) = reader.next_field() {
            // Field 2 is one Domain message. Field 1 is the name we already have.
            if let (2, Value::Bytes(domain)) = (number, value) {
                let mut kind = 0u64;
                let mut text = None;
                let mut inner = Reader::new(domain);
                while let Some((n, v)) = inner.next_field() {
                    match (n, v) {
                        (1, Value::Varint(t)) => kind = t,
                        (2, Value::Bytes(b)) => text = std::str::from_utf8(b).ok(),
                        _ => {}
                    }
                }
                if let Some(text) = text {
                    let text = text.trim_end_matches('.').to_ascii_lowercase();
                    match kind {
                        0 => set.keyword.push(text),
                        1 => set.skipped_regex += 1,
                        2 => {
                            set.suffix.insert(text);
                        }
                        3 => {
                            set.full.insert(text);
                        }
                        _ => {}
                    }
                }
            }
        }
        found.insert(code, set);
    }
    Ok(found)
}

/// The name of every category in `geosite.dat`, uppercased, without reading
/// one domain: enough to know a category exists before suggesting it.
pub fn site_names(path: &Path) -> std::io::Result<HashSet<String>> {
    let mut names = HashSet::new();
    walk(
        path,
        |name| {
            names.insert(name.to_owned());
            false
        },
        |_, _| {},
    )?;
    Ok(names)
}

/// Load the named countries from `geoip.dat`.
pub fn load_ips(path: &Path, wanted: &[String]) -> std::io::Result<HashMap<String, IpSet>> {
    let mut found = HashMap::new();
    if wanted.is_empty() {
        return Ok(found);
    }
    let wanted: HashSet<String> = wanted.iter().map(|w| w.to_ascii_uppercase()).collect();

    for (code, entry) in wanted_entries(path, &wanted)? {
        let mut set = IpSet::new();
        let mut reader = Reader::new(&entry);
        while let Some((number, value)) = reader.next_field() {
            if let (2, Value::Bytes(cidr)) = (number, value) {
                let mut addr = None;
                let mut prefix = 0u64;
                let mut inner = Reader::new(cidr);
                while let Some((n, v)) = inner.next_field() {
                    match (n, v) {
                        (1, Value::Bytes(raw)) => addr = ip_from_bytes(raw),
                        (2, Value::Varint(p)) => prefix = p,
                        _ => {}
                    }
                }
                if let Some(addr) = addr {
                    set.insert(addr, prefix.min(128) as u8);
                }
            }
        }
        found.insert(code, set);
    }
    Ok(found)
}

/// A `CIDR.ip` field: four bytes or sixteen, and nothing else is meaningful.
fn ip_from_bytes(raw: &[u8]) -> Option<IpAddr> {
    match raw.len() {
        4 => Some(IpAddr::V4(Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]))),
        16 => {
            let octets: [u8; 16] = raw.try_into().ok()?;
            Some(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        _ => None,
    }
}

/// The entries named in `wanted` (uppercased), each read whole.
fn wanted_entries(
    path: &Path,
    wanted: &HashSet<String>,
) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let mut found = Vec::new();
    walk(
        path,
        |name| wanted.contains(name),
        |name, entry| {
            found.push((name, entry));
        },
    )?;
    Ok(found)
}

/// Walk a `GeoSiteList` or `GeoIPList` on disk without reading it whole:
/// each entry's head is read for its name, `want` decides, and an entry
/// nobody asked for is stepped over. The files run to 90 MB (runetfreedom's
/// geosite), and every rule change on the phone read all of it into memory
/// to find a category or two: now a few hundred bytes an entry are read,
/// plus the entries asked for. A file that ends early or lies about a
/// length ends the walk with what was found, as a corrupt one always did.
fn walk(
    path: &Path,
    mut want: impl FnMut(&str) -> bool,
    mut take: impl FnMut(String, Vec<u8>),
) -> std::io::Result<()> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    // Small: the buffer is refilled after every step over an entry, and what
    // is wanted from each is its first few bytes.
    let mut file = std::io::BufReader::with_capacity(512, file);

    // A varint from the file, with the bytes it took; `None` at the end.
    fn varint(file: &mut impl BufRead, raw: &mut Vec<u8>) -> Option<u64> {
        let mut value = 0u64;
        for shift in 0..10 {
            let mut byte = [0u8; 1];
            file.read_exact(&mut byte).ok()?;
            raw.push(byte[0]);
            value |= u64::from(byte[0] & 0x7f) << (shift * 7);
            if byte[0] & 0x80 == 0 {
                return Some(value);
            }
        }
        None
    }

    let mut scratch = Vec::new();
    loop {
        scratch.clear();
        let Some(key) = varint(&mut file, &mut scratch) else {
            return Ok(());
        };
        let len = match key & 7 {
            0 => {
                if varint(&mut file, &mut scratch).is_none() {
                    return Ok(());
                }
                continue;
            }
            1 => 8,
            2 => match varint(&mut file, &mut scratch) {
                Some(len) => len,
                None => return Ok(()),
            },
            5 => 4,
            _ => return Ok(()),
        };
        if len > size {
            return Ok(());
        }
        if key != (1 << 3 | 2) {
            if file.seek_relative(len as i64).is_err() {
                return Ok(());
            }
            continue;
        }
        // An entry: its name is field 1, first in every file seen. Read that
        // much, and the rest only if it is wanted.
        let mut head = Vec::new();
        let named = match varint(&mut file, &mut head) {
            Some(k) if k == (1 << 3 | 2) => match varint(&mut file, &mut head) {
                Some(n) if (head.len() as u64 + n) <= len => {
                    let start = head.len();
                    head.resize(start + n as usize, 0);
                    if file.read_exact(&mut head[start..]).is_err() {
                        return Ok(());
                    }
                    std::str::from_utf8(&head[start..])
                        .ok()
                        .map(str::to_ascii_uppercase)
                }
                _ => None,
            },
            _ => None,
        };
        // The head's varints can run past an entry that says it is shorter
        // than they are: a corrupt file, which ends the walk like any other.
        let Some(rest) = len.checked_sub(head.len() as u64) else {
            return Ok(());
        };
        match named {
            Some(name) if !want(&name) => {
                if file.seek_relative(rest as i64).is_err() {
                    return Ok(());
                }
            }
            named => {
                // Wanted, or a file whose entries do not start with their
                // name: read the entry whole and look.
                let mut entry = head;
                let start = entry.len();
                entry.resize(start + rest as usize, 0);
                if file.read_exact(&mut entry[start..]).is_err() {
                    return Ok(());
                }
                match named {
                    Some(name) => take(name, entry),
                    None => {
                        if let Some(name) = entry_name(&entry).filter(|name| want(name)) {
                            take(name, entry);
                        }
                    }
                }
            }
        }
    }
}

/// Field 1 of an entry: its name, uppercased for comparison.
///
/// Read on its own so an entry nobody wants costs one string rather than the
/// hundred thousand domains inside it.
fn entry_name(entry: &[u8]) -> Option<String> {
    let mut reader = Reader::new(entry);
    while let Some((number, value)) = reader.next_field() {
        if let (1, Value::Bytes(name)) = (number, value) {
            return std::str::from_utf8(name)
                .ok()
                .map(|n| n.to_ascii_uppercase());
        }
    }
    None
}

// --- just enough protobuf -------------------------------------------------

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    /// Neither of these files contains a fixed-width field, but the decoder
    /// has to step over one rather than lose its place if a future one does.
    Fixed,
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut value = 0u64;
        for shift in 0..10 {
            let byte = *self.buf.get(self.pos)?;
            self.pos += 1;
            value |= u64::from(byte & 0x7f) << (shift * 7);
            if byte & 0x80 == 0 {
                return Some(value);
            }
        }
        // Eleven continuation bytes is not a number, it is a corrupt file.
        None
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(len)?;
        let slice = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }

    fn next_field(&mut self) -> Option<(u32, Value<'a>)> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let key = self.varint()?;
        let number = (key >> 3) as u32;
        let value = match key & 7 {
            0 => Value::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Value::Fixed
            }
            2 => {
                let len = self.varint()? as usize;
                Value::Bytes(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Value::Fixed
            }
            // Groups were removed from protobuf before these files existed.
            _ => return None,
        };
        Some((number, value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a length-delimited field by hand, so the decoder is tested against
    /// bytes rather than against itself.
    fn field(number: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = varint(u64::from(number) << 3 | 2);
        out.extend(varint(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    fn varint_field(number: u32, value: u64) -> Vec<u8> {
        let mut out = varint(u64::from(number) << 3);
        out.extend(varint(value));
        out
    }

    fn varint(mut value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn domain(kind: u64, value: &str) -> Vec<u8> {
        let mut inner = varint_field(1, kind);
        inner.extend(field(2, value.as_bytes()));
        field(2, &inner)
    }

    #[test]
    fn decodes_a_geosite_entry_and_keeps_only_what_was_asked_for() {
        let mut wanted = field(1, b"CATEGORY-ADS-ALL");
        wanted.extend(domain(2, "doubleclick.net"));
        wanted.extend(domain(3, "ads.example.com"));
        wanted.extend(domain(0, "adservice"));
        wanted.extend(domain(1, "^ad[0-9]+\\."));

        let mut ignored = field(1, b"GOOGLE");
        ignored.extend(domain(2, "google.com"));

        let mut file = field(1, &wanted);
        file.extend(field(1, &ignored));

        let dir = std::env::temp_dir().join("bliss-geo-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("geosite.dat");
        std::fs::write(&path, &file).unwrap();

        let sets = load_sites(&path, &["category-ads-all".to_owned()]).unwrap();
        assert_eq!(sets.len(), 1, "the category nobody asked for is not loaded");

        let ads = &sets["CATEGORY-ADS-ALL"];
        assert!(ads.suffix.contains("doubleclick.net"));
        assert!(ads.full.contains("ads.example.com"));
        assert_eq!(ads.keyword, ["adservice"]);
        // Regexes are counted rather than silently dropped: a category that is
        // mostly regex would match far less than its size suggests.
        assert_eq!(ads.skipped_regex, 1);
    }

    #[test]
    fn suffix_matching_walks_the_labels() {
        let mut set = SiteSet::default();
        set.suffix.insert("example.com".to_owned());

        assert!(set.matches("example.com"), "the name itself");
        assert!(set.matches("a.example.com"));
        assert!(set.matches("a.b.example.com"));
        // A trailing dot is the same name.
        assert!(set.matches("a.example.com."));
        assert!(set.matches("A.Example.COM"), "case is not part of a name");

        // The trap a naive `ends_with` falls into.
        assert!(!set.matches("notexample.com"));
        assert!(!set.matches("example.com.evil.net"));
        assert!(!set.matches("com"));
    }

    #[test]
    fn full_matches_exactly_and_keyword_matches_anywhere() {
        let mut set = SiteSet::default();
        set.full.insert("ads.example.com".to_owned());
        set.keyword.push("googlevideo".to_owned());

        assert!(set.matches("ads.example.com"));
        assert!(!set.matches("more.ads.example.com"), "full is not a suffix");
        assert!(set.matches("rr1---sn-x.googlevideo.com"));
    }

    #[test]
    fn cidr_blocks_match_only_inside_themselves() {
        let mut set = IpSet::new();
        set.insert("10.0.0.0".parse().unwrap(), 8);
        set.insert("192.168.1.0".parse().unwrap(), 24);
        set.insert("2001:db8::".parse().unwrap(), 32);

        assert!(set.matches("10.1.2.3".parse().unwrap()));
        assert!(set.matches("10.255.255.255".parse().unwrap()));
        assert!(!set.matches("11.0.0.1".parse().unwrap()));

        assert!(set.matches("192.168.1.77".parse().unwrap()));
        assert!(!set.matches("192.168.2.77".parse().unwrap()), "next /24");

        assert!(set.matches("2001:db8:1234::1".parse().unwrap()));
        assert!(!set.matches("2001:db9::1".parse().unwrap()));

        // A single address is a /32, and must not match its neighbour.
        let mut one = IpSet::new();
        one.insert("203.0.113.5".parse().unwrap(), 32);
        assert!(one.matches("203.0.113.5".parse().unwrap()));
        assert!(!one.matches("203.0.113.6".parse().unwrap()));
    }

    /// The decoder has to survive a file that is not what we expect, because the
    /// alternative is a client that will not start after the geo files are
    /// updated upstream.
    /// The walk over a real file finds every entry the whole file holds,
    /// byte for byte: run on the three sources the phone offers with
    /// `BLISS_GEO_DIR=<dir with geosite.dat and geoip.dat> cargo test -p
    /// bliss-core -- --ignored the_walk`.
    #[test]
    #[ignore]
    fn the_walk_reads_what_the_whole_file_holds() {
        let dir = std::path::PathBuf::from(std::env::var("BLISS_GEO_DIR").expect("BLISS_GEO_DIR"));
        for file in ["geosite.dat", "geoip.dat"] {
            let path = dir.join(file);
            let bytes = std::fs::read(&path).unwrap();
            let mut whole = std::collections::BTreeMap::new();
            let mut reader = Reader::new(&bytes);
            while let Some((number, value)) = reader.next_field() {
                if let (1, Value::Bytes(entry)) = (number, value) {
                    whole.insert(entry_name(entry).unwrap(), entry.to_vec());
                }
            }
            let names: HashSet<String> = whole.keys().cloned().collect();
            let mut walked = std::collections::BTreeMap::new();
            walk(
                &path,
                |_| true,
                |name, entry| {
                    walked.insert(name, entry);
                },
            )
            .unwrap();
            assert_eq!(walked.len(), whole.len(), "{file}");
            assert!(walked == whole, "{file}: an entry differs");
            if file == "geosite.dat" {
                assert_eq!(site_names(&path).unwrap(), names);
            }
            let some: HashSet<String> = names.iter().step_by(37).cloned().collect();
            let picked = wanted_entries(&path, &some).unwrap();
            assert_eq!(picked.len(), some.len(), "{file}");
            for (name, entry) in picked {
                assert_eq!(whole[&name], entry, "{file}: {name}");
            }
        }
    }

    #[test]
    fn a_corrupt_file_yields_nothing_rather_than_panicking() {
        let dir = std::env::temp_dir().join("bliss-geo-test");
        std::fs::create_dir_all(&dir).unwrap();

        for (name, bytes) in [
            ("truncated.dat", vec![0x0a, 0xff]),
            ("garbage.dat", vec![0xff; 64]),
            ("empty.dat", vec![]),
            // A length that runs off the end of the buffer.
            ("overlong.dat", vec![0x0a, 0x7f, 0x01]),
            // An entry shorter than the varint that starts it: the rest of
            // it came out below zero, and the walk panicked (found by
            // fuzzing on 26 November).
            ("short.dat", vec![0x0a, 0x00, 0x00, 0x00]),
            ("shorter.dat", vec![0x0a, 0x01, 0x0a, 0x05, b'R', b'U']),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, &bytes).unwrap();
            let sets = load_sites(&path, &["anything".to_owned()]).unwrap();
            assert!(sets.is_empty(), "{name} should decode to nothing");
        }
    }
}
