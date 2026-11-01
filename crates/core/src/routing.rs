//! Deciding what happens to each connection.
//!
//! The table is built once, from the profile plus whatever geo categories its
//! rules name, and then answers every connection without allocating or locking.
//! Rules are tried in order and the first match wins.
//!
//! Two properties this has to keep, because the routing screen is built on
//! them: **every decision names the rule that made it**, so "why did this go
//! direct?" always has an answer; and a rule that cannot work says so when the
//! table is built rather than by quietly never matching.

use std::net::IpAddr;
use std::path::Path;

use bliss_proto::{Action, Routing};

use crate::geo::{IpSet, SiteSet};
use crate::target::Target;
use crate::tracker::Route;

/// What to do with one connection, and what decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub route: Route,
    /// The rule's name, or `по умолчанию` when nothing matched.
    pub rule: String,
}

impl From<Action> for Route {
    fn from(action: Action) -> Self {
        match action {
            Action::Proxy => Route::Proxy,
            Action::Direct => Route::Direct,
            Action::Block => Route::Blocked,
            Action::Vpn => Route::Vpn,
        }
    }
}

/// A domain pattern, in the three shapes the config spells.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    /// This name and everything under it.
    Suffix(String),
    /// Exactly this name.
    Full(String),
    /// The text appears anywhere in the name.
    Keyword(String),
}

impl Pattern {
    fn parse(raw: &str) -> Self {
        let raw = raw.trim();
        if let Some(rest) = raw.strip_prefix("full:") {
            return Self::Full(normalise(rest));
        }
        if let Some(rest) = raw.strip_prefix("keyword:") {
            return Self::Keyword(normalise(rest));
        }
        // `suffix:` is accepted for symmetry with the other two, and for
        // anyone used to writing it out.
        if let Some(rest) = raw.strip_prefix("suffix:") {
            return Self::Suffix(normalise(rest));
        }
        if let Some(rest) = raw.strip_prefix("domain:") {
            return Self::Suffix(normalise(rest));
        }
        Self::Suffix(normalise(raw))
    }

    fn matches(&self, host: &str) -> bool {
        match self {
            Self::Full(name) => host == name,
            Self::Keyword(text) => host.contains(text.as_str()),
            Self::Suffix(name) => {
                // Label-wise, so `example.com` does not match `notexample.com`.
                host == name
                    || host
                        .strip_suffix(name.as_str())
                        .is_some_and(|head| head.ends_with('.'))
            }
        }
    }
}

fn normalise(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// A port or a range of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PortRange(u16, u16);

impl PortRange {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        match raw.split_once('-') {
            Some((low, high)) => {
                let low = low.trim().parse().ok()?;
                let high = high.trim().parse().ok()?;
                // Written backwards is a typo, not an empty range: read it the
                // way it was obviously meant.
                Some(if low <= high {
                    Self(low, high)
                } else {
                    Self(high, low)
                })
            }
            None => {
                let one = raw.parse().ok()?;
                Some(Self(one, one))
            }
        }
    }

    fn contains(self, port: u16) -> bool {
        (self.0..=self.1).contains(&port)
    }
}

/// A block of addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cidr {
    net: IpAddr,
    prefix: u8,
}

impl Cidr {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let (addr, prefix) = match raw.split_once('/') {
            Some((addr, prefix)) => (addr, prefix.trim().parse::<u8>().ok()?),
            // A bare address is that address alone.
            None => (raw, if raw.contains(':') { 128 } else { 32 }),
        };
        let net: IpAddr = addr.trim().parse().ok()?;
        let width = if net.is_ipv4() { 32 } else { 128 };
        (prefix <= width).then_some(Self { net, prefix })
    }

    /// Whether a public IPv4 address could fall in this block: false for a
    /// v6 block (a site's address is looked up as IPv4 only) and for one
    /// wholly inside the private and special ranges, where public DNS sends
    /// no site.
    fn may_hold_a_site(&self) -> bool {
        const PRIVATE: [(u32, u8); 8] = [
            (0x0000_0000, 8),  // 0.0.0.0/8
            (0x0a00_0000, 8),  // 10.0.0.0/8
            (0x6440_0000, 10), // 100.64.0.0/10
            (0x7f00_0000, 8),  // 127.0.0.0/8
            (0xa9fe_0000, 16), // 169.254.0.0/16
            (0xac10_0000, 12), // 172.16.0.0/12
            (0xc0a8_0000, 16), // 192.168.0.0/16
            (0xe000_0000, 3),  // multicast and reserved, 224.0.0.0/3
        ];
        let IpAddr::V4(net) = self.net else {
            return false;
        };
        let net = u32::from(net);
        !PRIVATE
            .iter()
            .any(|&(range, prefix)| self.prefix >= prefix && masked_v4(net, prefix) == range)
    }

    fn contains(&self, ip: IpAddr) -> bool {
        match (self.net, ip) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                masked_v4(u32::from(net), self.prefix) == masked_v4(u32::from(ip), self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                masked_v6(u128::from(net), self.prefix) == masked_v6(u128::from(ip), self.prefix)
            }
            // A v4 rule never matches a v6 address, and the reverse.
            _ => false,
        }
    }
}

fn masked_v4(bits: u32, prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        bits & (u32::MAX << (32 - u32::from(prefix)))
    }
}

fn masked_v6(bits: u128, prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        bits & (u128::MAX << (128 - u32::from(prefix)))
    }
}

/// One rule, compiled.
struct Compiled {
    name: String,
    route: Route,
    domains: Vec<Pattern>,
    cidrs: Vec<Cidr>,
    ports: Vec<PortRange>,
    processes: Vec<String>,
    sites: Vec<SiteSet>,
    ips: Vec<IpSet>,
    /// The geoip countries it names, as written, apart from `private`: for
    /// saying why a site went past it, never for deciding.
    countries: Vec<String>,
    /// Whether a site's public IPv4 address could meet one of its address
    /// conditions: a country, or a block outside the private and special
    /// ranges. «Локальная сеть» and the IPv6 refusal cannot, and are no
    /// reason to look a site's address up (`Router::wants_address`).
    sees_sites: bool,
    /// For UDP flows only (`Some(true)`), TCP only (`Some(false)`), or both.
    udp_only: Option<bool>,
    /// Nothing but the network was written: every flow of it matches.
    network_alone: bool,
}

impl Compiled {
    fn matches(
        &self,
        host: &str,
        ip: Option<IpAddr>,
        port: u16,
        process: Option<&str>,
        udp: bool,
    ) -> bool {
        // The network limits every other condition, as in xray.
        if self.udp_only.is_some_and(|only| only != udp) {
            return false;
        }
        if self.network_alone {
            return true;
        }
        if self.domains.iter().any(|p| p.matches(host)) {
            return true;
        }
        if self.sites.iter().any(|s| s.matches(host)) {
            return true;
        }
        if let Some(ip) = ip {
            if self.cidrs.iter().any(|c| c.contains(ip)) {
                return true;
            }
            if self.ips.iter().any(|s| s.matches(ip)) {
                return true;
            }
        }
        if self.ports.iter().any(|r| r.contains(port)) {
            return true;
        }
        if let Some(process) = process
            && self.processes.contains(&program_name(process))
        {
            return true;
        }
        false
    }
}

/// A program's name as rules compare it: without case, and without the
/// `.exe` Windows names its programs with. A profile carried from Windows to
/// Linux says `Telegram.exe` where Linux says `Telegram`, and the other way
/// round a rule written on Linux would never match on Windows.
fn program_name(name: &str) -> String {
    let name = name.trim().to_ascii_lowercase();
    match name.strip_suffix(".exe") {
        Some(stem) if !stem.is_empty() => stem.to_owned(),
        _ => name,
    }
}

/// The compiled rule table.
pub struct Router {
    rules: Vec<Compiled>,
    fallback: Route,
    /// Things worth telling the user about the table, gathered while building
    /// it: a category that is not in the geo files, a pattern that does not
    /// parse, a rule that matches nothing. Reported once at startup rather than
    /// discovered by the traffic going somewhere unexpected.
    pub warnings: Vec<String>,
}

impl Router {
    /// A router that sends everything through the tunnel, as bliss did before
    /// there were rules.
    pub fn proxy_everything() -> Self {
        Self {
            rules: Vec::new(),
            fallback: Route::Proxy,
            warnings: Vec::new(),
        }
    }

    /// Compile a profile's rules, loading whichever geo categories they name.
    ///
    /// `geo_dir` is where `geosite.dat` and `geoip.dat` live. A missing file is
    /// a warning rather than a failure: the rules that do not need it still
    /// work, and a client that refuses to start because a database is absent is
    /// worse than one that tells you which rules are inactive.
    pub fn build(config: &Routing, geo_dir: &Path) -> Self {
        Self::compile(config, geo_dir, false)
    }

    /// The same table for a caller that learns a site's address before
    /// deciding (`decide_with`) — Android's TUN, which resolves names through
    /// the tunnel — so a country rule does see sites, and the warning that it
    /// cannot is not given.
    pub fn build_resolving(config: &Routing, geo_dir: &Path) -> Self {
        Self::compile(config, geo_dir, true)
    }

    fn compile(config: &Routing, geo_dir: &Path, resolving: bool) -> Self {
        let mut warnings = Vec::new();

        // Load every category any rule mentions, once, rather than per rule —
        // and none for a rule that is switched off, which decides nothing and
        // is no reason to hold a country's addresses in memory.
        let wanted_sites: Vec<String> = config
            .rules
            .iter()
            .filter(|r| r.enabled)
            .flat_map(|r| r.geosite.iter().cloned())
            .collect();
        let wanted_ips: Vec<String> = config
            .rules
            .iter()
            .filter(|r| r.enabled)
            .flat_map(|r| r.geoip.iter().cloned())
            .collect();

        let sites = load_or_warn(
            "geosite.dat",
            geo_dir,
            &wanted_sites,
            &mut warnings,
            crate::geo::load_sites,
        );
        let ips = load_or_warn(
            "geoip.dat",
            geo_dir,
            &wanted_ips,
            &mut warnings,
            crate::geo::load_ips,
        );

        // Read only when a rule needs a category suggested; see below.
        let mut site_names: Option<Option<std::collections::HashSet<String>>> = None;

        let mut rules = Vec::with_capacity(config.rules.len());
        // Numbered by place in the whole list, switched-off rules counted, so
        // "#3" in a warning is the third row on the screen.
        for (index, rule) in config.rules.iter().enumerate() {
            // A 0.2.6 rule's `protocol = ["quic"]`, read as UDP to port 443.
            let settled;
            let rule = if rule.protocol.is_empty() {
                rule
            } else {
                let mut copy = rule.clone();
                copy.settle_protocol();
                settled = copy;
                &settled
            };
            if !rule.enabled {
                continue;
            }
            let name = rule.display_name();
            let where_ = format!("правило #{} ({name})", index + 1);

            if rule.is_empty() {
                warnings.push(format!(
                    "{where_}: нет ни одного условия, правило пропущено"
                ));
                continue;
            }

            let mut compiled = Compiled {
                name,
                route: rule.action.into(),
                domains: rule.domain.iter().map(|d| Pattern::parse(d)).collect(),
                cidrs: Vec::new(),
                ports: Vec::new(),
                processes: rule.process.iter().map(|p| program_name(p)).collect(),
                sites: Vec::new(),
                ips: Vec::new(),
                countries: rule
                    .geoip
                    .iter()
                    .filter(|country| !country.eq_ignore_ascii_case("private"))
                    .cloned()
                    .collect(),
                sees_sites: false,
                udp_only: {
                    let udp = rule.network.iter().any(|n| n.eq_ignore_ascii_case("udp"));
                    let tcp = rule.network.iter().any(|n| n.eq_ignore_ascii_case("tcp"));
                    (udp != tcp).then_some(udp)
                },
                network_alone: !rule.network.is_empty()
                    && rule.domain.is_empty()
                    && rule.ip.is_empty()
                    && rule.port.is_empty()
                    && rule.process.is_empty()
                    && rule.geosite.is_empty()
                    && rule.geoip.is_empty(),
            };

            for raw in &rule.ip {
                match Cidr::parse(raw) {
                    Some(cidr) => compiled.cidrs.push(cidr),
                    None => warnings.push(format!("{where_}: {raw:?} — это не адрес и не подсеть")),
                }
            }
            for raw in &rule.port {
                match PortRange::parse(raw) {
                    Some(range) => compiled.ports.push(range),
                    None => warnings.push(format!("{where_}: {raw:?} — это не порт и не диапазон")),
                }
            }
            for raw in &rule.geosite {
                match sites.get(&raw.to_ascii_uppercase()) {
                    Some(set) => {
                        if set.skipped_regex > 0 {
                            warnings.push(format!(
                                "{where_}: в наборе {raw} есть {} шаблон(ов) вида regex, \
                                 их bliss пока не умеет",
                                set.skipped_regex
                            ));
                        }
                        compiled.sites.push(set.clone());
                    }
                    None => warnings.push(format!("{where_}: нет набора geosite с именем {raw:?}")),
                }
            }
            for raw in &rule.geoip {
                match ips.get(&raw.to_ascii_uppercase()) {
                    Some(set) => compiled.ips.push(set.clone()),
                    None => warnings.push(format!("{where_}: нет страны geoip с именем {raw:?}")),
                }
            }
            // A country's addresses are not its sites. A browser behind the proxy
            // names the site, bliss never resolves the name (the note on
            // `Rule::geoip`), and a rule of geoip alone quietly never matched a
            // single page — which is how the user found out. So the table says
            // it, and names the category that does catch those sites when
            // geosite.dat has one.
            let blind: Vec<&String> = compiled
                .countries
                .iter()
                .filter(|country| !rule.geosite.iter().any(|set| covers_country(set, country)))
                .collect();
            if !blind.is_empty() && !resolving {
                let names = site_names.get_or_insert_with(|| {
                    crate::geo::site_names(&geo_dir.join("geosite.dat")).ok()
                });
                warnings.push(blind_warning(&where_, &blind, names.as_ref()));
            }

            compiled.sees_sites = !compiled.countries.is_empty()
                || compiled.cidrs.iter().any(|cidr| cidr.may_hold_a_site());
            rules.push(compiled);
        }

        Self {
            rules,
            fallback: config.fallback.into(),
            warnings,
        }
    }

    /// Which way does this connection go, and why?
    pub fn decide(&self, target: &Target, process: Option<&str>) -> Decision {
        self.decide_with(target, None, process)
    }

    /// `decide`, for a site whose address the caller has learnt: domain and
    /// geosite conditions look at the name, address and geoip conditions at
    /// `resolved`. Only a caller that resolved the name without leaking it —
    /// through the tunnel — should pass one (AGENTS.md §5).
    pub fn decide_with(
        &self,
        target: &Target,
        resolved: Option<IpAddr>,
        process: Option<&str>,
    ) -> Decision {
        self.decide_flow(target, resolved, process, false)
    }

    /// `decide_with`, for a TCP connection or (`udp`) a flow of datagrams,
    /// which a rule's `network` tells apart. `decide` and `decide_with` are
    /// for TCP: the SOCKS and HTTP ports carry nothing else.
    pub fn decide_flow(
        &self,
        target: &Target,
        resolved: Option<IpAddr>,
        process: Option<&str>,
        udp: bool,
    ) -> Decision {
        let host = normalise(&target.host());
        let ip = match target {
            Target::Socket(addr) => Some(addr.ip()),
            // A name that is spelled as an address *is* an address, and has to
            // match address rules. Clients do this: SOCKS5 has a type for
            // literal addresses and plenty of programs send them through the
            // domain one anyway, and `CONNECT 1.2.3.4:443` arrives as text. A
            // rule blocking a block of addresses that quietly failed to match
            // because the client typed it rather than packed it would be a
            // routing bypass nobody could see.
            //
            // A name that is not an address is still never resolved here — see
            // the note on `Rule::geoip`.
            Target::Domain { host, .. } => host.parse().ok().or(resolved),
        };
        let port = target.port();

        for rule in &self.rules {
            if rule.matches(&host, ip, port, process, udp) {
                return Decision {
                    route: rule.route,
                    rule: rule.name.clone(),
                };
            }
        }
        Decision {
            route: self.fallback,
            rule: "по умолчанию".to_owned(),
        }
    }

    /// Whether a site's address could change the decision: walking the rules
    /// by the name alone, an address or country rule comes before any rule
    /// that matches. False for an address, which is its own answer, and for a
    /// name decided before any such rule — so a name is resolved only when an
    /// answer depends on it, as xray's `IPIfNonMatch` does.
    pub fn wants_address(&self, target: &Target, process: Option<&str>, udp: bool) -> bool {
        let Target::Domain { host, .. } = target else {
            return false;
        };
        if host.parse::<IpAddr>().is_ok() {
            return false;
        }
        let host = normalise(host);
        let port = target.port();
        for rule in &self.rules {
            if rule.matches(&host, None, port, process, udp) {
                return false;
            }
            if rule.sees_sites && rule.udp_only.is_none_or(|only| only == udp) {
                return true;
            }
        }
        false
    }

    /// Whether any rule could decide a site by its public address.
    pub fn has_address_rules(&self) -> bool {
        self.rules.iter().any(|rule| rule.sees_sites)
    }

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub fn fallback(&self) -> Route {
        self.fallback
    }

    /// Why no rule with a country decided a destination that names a site, in
    /// words for the user; `None` for an address, or when no such rule came
    /// before the decision. It explains a decision and never makes one.
    pub fn geoip_note(&self, target: &Target, process: Option<&str>) -> Option<String> {
        let passed = self.geoip_passed_over(target, process);
        let names = passed
            .iter()
            .map(|name| format!("«{name}»"))
            .collect::<Vec<_>>()
            .join(", ");
        match passed.len() {
            0 => None,
            1 => Some(format!(
                "{} — имя сайта, а geoip сравнивает адреса: правило {names} его не проверяло",
                target.host()
            )),
            _ => Some(format!(
                "{} — имя сайта, а geoip сравнивает адреса: правила {names} его не проверяли",
                target.host()
            )),
        }
    }

    /// The rules with a country that a destination went past before its
    /// decision, their geoip conditions never looking at it because it is a
    /// name. Empty for an address, which geoip does see.
    fn geoip_passed_over(&self, target: &Target, process: Option<&str>) -> Vec<String> {
        let named = match target {
            Target::Socket(_) => false,
            Target::Domain { host, .. } => host.parse::<IpAddr>().is_err(),
        };
        if !named {
            return Vec::new();
        }
        let host = normalise(&target.host());
        let port = target.port();
        self.rules
            .iter()
            .take_while(|rule| !rule.matches(&host, None, port, process, false))
            .filter(|rule| !rule.countries.is_empty())
            .map(|rule| rule.name.clone())
            .collect()
    }
}

/// Whether a geosite category is a country's sites, in the names the common
/// files give them: `category-ru`, `cn`, `geolocation-cn`, `tld-ru`.
fn covers_country(set: &str, country: &str) -> bool {
    let set = set.trim().to_ascii_lowercase();
    let country = country.trim().to_ascii_lowercase();
    ["", "category-", "geolocation-", "tld-"]
        .iter()
        .any(|prefix| set == format!("{prefix}{country}"))
}

/// The warning for countries in a rule that cannot see a site's name, with
/// the category to add for each one geosite.dat has.
fn blind_warning(
    where_: &str,
    blind: &[&String],
    names: Option<&std::collections::HashSet<String>>,
) -> String {
    let conditions = blind
        .iter()
        .map(|country| format!("geoip:{country}"))
        .collect::<Vec<_>>()
        .join(", ");
    let suggested: Vec<String> = blind
        .iter()
        .filter_map(|country| {
            let names = names?;
            let country = country.to_ascii_lowercase();
            ["category-", "", "geolocation-", "tld-"]
                .iter()
                .map(|prefix| format!("{prefix}{country}"))
                .find(|set| names.contains(&set.to_ascii_uppercase()))
                .map(|set| format!("geosite:{set}"))
        })
        .collect();
    let head = format!(
        "{where_}: {conditions} {} адреса, а браузер через прокси называет сайт по имени, \
         так что сайты это правило не поймает.",
        if blind.len() == 1 {
            "сравнивает"
        } else {
            "сравнивают"
        }
    );
    match suggested.len() {
        0 => format!("{head} Их ловят условия geosite и домены."),
        1 => format!(
            "{head} Их ловит {} — добавьте его в это же правило.",
            suggested[0]
        ),
        _ => format!(
            "{head} Их ловят {} — добавьте их в это же правило.",
            suggested.join(", ")
        ),
    }
}

/// A geo loader: one of `geo::load_sites` or `geo::load_ips`.
type Categories<T> = std::collections::HashMap<String, T>;
type Loader<T> = fn(&Path, &[String]) -> std::io::Result<Categories<T>>;

fn load_or_warn<T>(
    file: &str,
    dir: &Path,
    wanted: &[String],
    warnings: &mut Vec<String>,
    load: Loader<T>,
) -> Categories<T> {
    if wanted.is_empty() {
        return Default::default();
    }
    let path = dir.join(file);
    match load(&path, wanted) {
        Ok(found) => found,
        Err(e) => {
            warnings.push(format!(
                "не удалось прочитать {} ({e}) — правила, которые его используют, \
                 не будут срабатывать",
                path.display()
            ));
            Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use bliss_proto::Rule;

    use super::*;

    fn rule(action: Action, name: &str) -> Rule {
        Rule {
            name: Some(name.to_owned()),
            action,
            ..Rule::default()
        }
    }

    /// `network` limits a rule rather than adding to its conditions, as in
    /// xray: UDP and port 443 is QUIC, never HTTPS on the same port. The
    /// phone's built-in rule, and how the user asked for it.
    #[test]
    fn udp_and_443_is_quic_not_https() {
        let mut quic = rule(Action::Block, "QUIC");
        quic.network = vec!["udp".into()];
        quic.port = vec!["443".into()];
        let mut site = rule(Action::Direct, "Reddit");
        site.domain = vec!["reddit.com".into()];
        let r = router(vec![site, quic], Action::Proxy);
        let page = Target::Domain {
            host: "github.com".into(),
            port: 443,
        };
        assert_eq!(r.decide_flow(&page, None, None, true).route, Route::Blocked);
        // HTTPS to the same place is TCP.
        assert_eq!(r.decide_flow(&page, None, None, false).route, Route::Proxy);
        assert_eq!(r.decide(&page, None).route, Route::Proxy);
        // UDP to another port is not QUIC.
        let call = Target::Socket("203.0.113.9:3478".parse().unwrap());
        assert_eq!(r.decide_flow(&call, None, None, true).route, Route::Proxy);
        // A rule above it decides first: the user's own site keeps its way.
        let reddit = Target::Domain {
            host: "www.reddit.com".into(),
            port: 443,
        };
        assert_eq!(
            r.decide_flow(&reddit, None, None, true).route,
            Route::Direct
        );
        // A network alone is every flow of it.
        let mut all_udp = rule(Action::Block, "UDP");
        all_udp.network = vec!["udp".into()];
        let r = router(vec![all_udp], Action::Proxy);
        assert_eq!(r.decide_flow(&call, None, None, true).route, Route::Blocked);
        assert_eq!(r.decide_flow(&page, None, None, false).route, Route::Proxy);
        // 0.2.6's protocol = ["quic"] is read as the same rule.
        let mut old = rule(Action::Block, "QUIC");
        old.protocol = vec!["quic".into()];
        old.settle_protocol();
        assert_eq!(
            (old.network.as_slice(), old.port.as_slice()),
            (&["udp".to_owned()][..], &["443".to_owned()][..])
        );
        assert!(old.protocol.is_empty());
    }

    /// A site's address, learnt by the caller, decides the address rules a
    /// name alone went past — and is asked for only when one comes before
    /// the decision. On Android every site arrives by name, and without this
    /// no `geoip:` rule ever matched a page there.

    #[test]
    fn the_local_network_and_ipv6_are_no_reason_to_look_a_site_up() {
        // The phone's first rule, a user's site rule, and the IPv6 refusal
        // the phone adds last: none of them can meet a site's public IPv4
        // address, so no site is looked up for them. 0.2.3 looked every
        // site up, and each new name waited on it (GitHub's assets, the user's
        // report).
        let mut lan = rule(Action::Direct, "Локальная сеть");
        lan.ip = vec![
            "10.0.0.0/8".into(),
            "192.168.0.0/16".into(),
            "fe80::/10".into(),
        ];
        lan.geoip = vec!["private".into()];
        let mut sites = rule(Action::Direct, "Reddit");
        sites.domain = vec!["reddit.com".into()];
        let mut v6 = rule(Action::Block, "IPv6");
        v6.ip = vec!["::/0".into()];
        let r = router(vec![lan, sites, v6], Action::Proxy);
        let site = Target::Domain {
            host: "github.githubassets.com".into(),
            port: 443,
        };
        assert!(!r.wants_address(&site, None, false));
        assert!(!r.has_address_rules());
        // A public block still asks.
        let mut far = rule(Action::Direct, "Сеть");
        far.ip = vec!["203.0.113.0/24".into()];
        let r = router(vec![far], Action::Proxy);
        assert!(r.wants_address(&site, None, false));
    }

    #[test]
    fn a_resolved_site_meets_the_address_rules() {
        let mut named = rule(Action::Proxy, "Сайт");
        named.domain = vec!["kept.example".into()];
        let mut lan = rule(Action::Direct, "Сеть");
        lan.ip = vec!["203.0.113.0/24".into()];
        let r = router(vec![named, lan], Action::Proxy);
        let site = Target::Domain {
            host: "shop.example".into(),
            port: 443,
        };
        let inside: IpAddr = "203.0.113.7".parse().unwrap();
        let outside: IpAddr = "198.51.100.5".parse().unwrap();
        assert!(r.wants_address(&site, None, false));
        assert_eq!(
            r.decide_with(&site, Some(inside), None).route,
            Route::Direct
        );
        assert_eq!(
            r.decide_with(&site, Some(outside), None).route,
            Route::Proxy
        );
        assert_eq!(r.decide(&site, None).route, Route::Proxy);
        // Decided by its name first: nothing to ask.
        let kept = Target::Domain {
            host: "kept.example".into(),
            port: 443,
        };
        assert!(!r.wants_address(&kept, None, false));
        assert_eq!(r.decide_with(&kept, Some(inside), None).route, Route::Proxy);
        // An address is its own answer.
        assert!(!r.wants_address(&Target::Socket("10.1.2.3:80".parse().unwrap()), None, false));
        assert!(r.has_address_rules());
    }

    fn router(rules: Vec<Rule>, fallback: Action) -> Router {
        Router::build(
            &Routing {
                fallback,
                rules,
                ..Routing::default()
            },
            Path::new("nonexistent"),
        )
    }

    /// A switched-off rule keeps its row and decides nothing; the rules after
    /// it keep their numbers, so a warning's "#2" is still the second row.
    #[test]
    fn a_switched_off_rule_decides_nothing() {
        let mut off = rule(Action::Block, "off");
        off.domain = vec!["example.com".into()];
        off.enabled = false;
        let empty = rule(Action::Direct, "empty");
        let r = router(vec![off, empty], Action::Proxy);

        let d = r.decide(&Target::domain("example.com", 443), None);
        assert_eq!((d.route, d.rule.as_str()), (Route::Proxy, "по умолчанию"));
        assert_eq!(r.rule_count(), 0);
        assert!(
            r.warnings.iter().any(|w| w.contains("#2 (empty)")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn a_domain_suffix_matches_the_name_and_everything_under_it() {
        let mut direct = rule(Action::Direct, "local");
        direct.domain = vec!["example.com".into()];
        let r = router(vec![direct], Action::Proxy);

        for host in ["example.com", "a.example.com", "a.b.example.com"] {
            let d = r.decide(&Target::domain(host, 443), None);
            assert_eq!(d.route, Route::Direct, "{host}");
            assert_eq!(d.rule, "local");
        }

        // The two names a careless `ends_with` would catch by mistake.
        for host in ["notexample.com", "example.com.evil.net"] {
            assert_eq!(
                r.decide(&Target::domain(host, 443), None).route,
                Route::Proxy,
                "{host}"
            );
        }
    }

    #[test]
    fn full_and_keyword_mean_what_they_say() {
        let mut exact = rule(Action::Block, "exact");
        exact.domain = vec!["full:ads.example.com".into()];
        let mut anywhere = rule(Action::Direct, "anywhere");
        anywhere.domain = vec!["keyword:googlevideo".into()];
        let r = router(vec![exact, anywhere], Action::Proxy);

        assert_eq!(
            r.decide(&Target::domain("ads.example.com", 443), None)
                .route,
            Route::Blocked
        );
        // full is not a suffix.
        assert_eq!(
            r.decide(&Target::domain("more.ads.example.com", 443), None)
                .route,
            Route::Proxy
        );
        assert_eq!(
            r.decide(&Target::domain("rr1.googlevideo.com", 443), None)
                .route,
            Route::Direct
        );
    }

    /// The ordering rule, and the reason it is the only one: exceptions go
    /// above the general case, and nothing else needs explaining.
    #[test]
    fn the_first_matching_rule_wins() {
        let mut exception = rule(Action::Proxy, "this one through the tunnel");
        exception.domain = vec!["news.example.com".into()];
        let mut general = rule(Action::Direct, "the rest of the domain direct");
        general.domain = vec!["example.com".into()];

        let r = router(vec![exception, general], Action::Block);
        assert_eq!(
            r.decide(&Target::domain("news.example.com", 443), None)
                .rule,
            "this one through the tunnel"
        );
        assert_eq!(
            r.decide(&Target::domain("other.example.com", 443), None)
                .rule,
            "the rest of the domain direct"
        );
        // And the fallback still applies to everything else.
        let elsewhere = r.decide(&Target::domain("unrelated.net", 443), None);
        assert_eq!(elsewhere.route, Route::Blocked);
        assert_eq!(elsewhere.rule, "по умолчанию");
    }

    #[test]
    fn addresses_match_blocks_and_single_hosts() {
        let mut lan = rule(Action::Direct, "lan");
        lan.ip = vec![
            "10.0.0.0/8".into(),
            "203.0.113.5".into(),
            "2001:db8::/32".into(),
        ];
        let r = router(vec![lan], Action::Proxy);

        let direct = |s: &str| r.decide(&Target::Socket(s.parse().unwrap()), None).route;
        assert_eq!(direct("10.1.2.3:80"), Route::Direct);
        assert_eq!(direct("203.0.113.5:80"), Route::Direct);
        assert_eq!(direct("203.0.113.6:80"), Route::Proxy, "a bare ip is a /32");
        assert_eq!(direct("[2001:db8::1]:80"), Route::Direct);
        assert_eq!(direct("11.0.0.1:80"), Route::Proxy);
    }

    /// A v4 rule must not match a v6 address. Getting this wrong sends traffic
    /// somewhere the user did not ask for, in the direction that leaks.
    #[test]
    fn address_families_do_not_match_each_other() {
        let mut v4 = rule(Action::Direct, "v4 only");
        v4.ip = vec!["0.0.0.0/0".into()];
        let r = router(vec![v4], Action::Proxy);

        assert_eq!(
            r.decide(&Target::Socket("1.2.3.4:80".parse().unwrap()), None)
                .route,
            Route::Direct
        );
        assert_eq!(
            r.decide(&Target::Socket("[2001:db8::1]:80".parse().unwrap()), None)
                .route,
            Route::Proxy,
            "a v4 block must not swallow v6"
        );
    }

    /// An address a client spelled out as a name still has to match address
    /// rules. Otherwise a block rule is bypassed by how the client happens to
    /// encode its destination, which is a hole nobody would ever see.
    #[test]
    fn an_address_sent_as_a_name_is_still_an_address() {
        let mut lan = rule(Action::Block, "lan");
        lan.ip = vec!["10.0.0.0/8".into(), "2001:db8::/32".into()];
        let r = router(vec![lan], Action::Proxy);

        // Packed, as SOCKS5's address type 1 delivers it.
        assert_eq!(
            r.decide(&Target::Socket("10.1.2.3:80".parse().unwrap()), None)
                .route,
            Route::Blocked
        );
        // Spelled, as SOCKS5's domain type and HTTP CONNECT deliver it.
        assert_eq!(
            r.decide(&Target::domain("10.1.2.3", 80), None).route,
            Route::Blocked,
            "a literal address sent as a name must not slip past an ip rule"
        );
        assert_eq!(
            r.decide(&Target::domain("2001:db8::1", 80), None).route,
            Route::Blocked
        );
        // And a name that merely looks numeric is still a name.
        assert_eq!(
            r.decide(&Target::domain("10.1.2.3.example.com", 80), None)
                .route,
            Route::Proxy
        );
    }

    #[test]
    fn ports_match_singly_and_in_ranges() {
        let mut blocked = rule(Action::Block, "smtp");
        blocked.port = vec!["25".into(), "6881-6889".into()];
        let r = router(vec![blocked], Action::Proxy);

        let route = |port| r.decide(&Target::domain("x.example", port), None).route;
        assert_eq!(route(25), Route::Blocked);
        assert_eq!(route(6881), Route::Blocked);
        assert_eq!(route(6885), Route::Blocked);
        assert_eq!(route(6889), Route::Blocked);
        assert_eq!(route(6890), Route::Proxy);
        assert_eq!(route(443), Route::Proxy);
    }

    #[test]
    fn a_range_written_backwards_is_read_the_way_it_was_meant() {
        assert_eq!(PortRange::parse("100-200"), PortRange::parse("200-100"));
        assert!(PortRange::parse("100-200").unwrap().contains(150));
        assert!(PortRange::parse("nonsense").is_none());
    }

    #[test]
    fn processes_match_by_name_whatever_the_case() {
        let mut torrent = rule(Action::Direct, "torrent");
        torrent.process = vec!["qBittorrent.exe".into()];
        let r = router(vec![torrent], Action::Proxy);

        let t = Target::domain("tracker.example", 6881);
        assert_eq!(r.decide(&t, Some("qbittorrent.exe")).route, Route::Direct);
        assert_eq!(r.decide(&t, Some("QBITTORRENT.EXE")).route, Route::Direct);
        assert_eq!(r.decide(&t, Some("chrome.exe")).route, Route::Proxy);
        // An unnamed process must not match a process rule by accident.
        assert_eq!(r.decide(&t, None).route, Route::Proxy);
    }

    /// A profile carried between Windows and Linux keeps its program rules:
    /// `.exe` is Windows' way of naming a program, not part of the program.
    #[test]
    fn a_program_is_the_same_program_with_or_without_exe() {
        let mut chat = rule(Action::Direct, "chat");
        chat.process = vec!["Telegram.exe".into(), "discord".into()];
        let r = router(vec![chat], Action::Proxy);

        let t = Target::domain("chat.example", 443);
        assert_eq!(
            r.decide(&t, Some("Telegram")).route,
            Route::Direct,
            "on Linux"
        );
        assert_eq!(r.decide(&t, Some("telegram.exe")).route, Route::Direct);
        assert_eq!(
            r.decide(&t, Some("Discord.exe")).route,
            Route::Direct,
            "on Windows"
        );
        assert_eq!(r.decide(&t, Some("discord")).route, Route::Direct);
        assert_eq!(r.decide(&t, Some("telegram-desktop")).route, Route::Proxy);
        assert_eq!(
            r.decide(&t, Some(".exe")).route,
            Route::Proxy,
            "no name at all"
        );
    }

    /// A rule that cannot work is reported when the table is built, rather
    /// than found out from the traffic.
    #[test]
    fn a_rule_that_cannot_work_is_reported() {
        let mut nothing = rule(Action::Direct, "empty");
        let mut bad = rule(Action::Direct, "typos");
        bad.ip = vec!["not-an-address".into()];
        bad.port = vec!["http".into()];
        bad.geosite = vec!["no-such-category".into()];
        nothing.name = Some("empty".into());

        let r = router(vec![nothing, bad], Action::Proxy);
        assert_eq!(r.rule_count(), 1, "the conditionless rule is dropped");

        let all = r.warnings.join("\n");
        assert!(all.contains("нет ни одного условия"), "{all}");
        assert!(all.contains("not-an-address"), "{all}");
        assert!(all.contains("\"http\""), "{all}");
        // The geo file is absent here, so the missing database is the warning.
        assert!(all.contains("geosite.dat"), "{all}");
    }

    #[test]
    fn with_no_rules_everything_goes_through_the_tunnel() {
        let r = Router::proxy_everything();
        let d = r.decide(&Target::domain("anything.example", 443), None);
        assert_eq!(d.route, Route::Proxy);
        assert_eq!(r.rule_count(), 0);
    }

    /// A geoip rule cannot see a site's name. The user found that out from
    /// Russian sites still going through the tunnel under `geoip:ru`; the table
    /// now says so when it is built, naming the category that catches those
    /// sites, and the answer for a name says which rules never looked at it.
    #[test]
    fn a_country_rule_says_it_cannot_see_sites() {
        // A geosite.dat of one category, `category-ru`, which covers `ru`.
        let field = |number: u8, bytes: &[u8]| {
            let mut out = vec![(number << 3) | 2, bytes.len() as u8];
            out.extend_from_slice(bytes);
            out
        };
        let domain = [vec![0x08, 0x02], field(2, b"ru")].concat();
        let entry = [field(1, b"CATEGORY-RU"), field(2, &domain)].concat();
        let dir = std::env::temp_dir()
            .join("bliss-routing-tests")
            .join("country-sites");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("geosite.dat"), field(1, &entry)).expect("geosite.dat");

        let mut sites = rule(Action::Direct, "Сайты");
        sites.domain = vec!["yandex.ru".into()];
        let mut lan = rule(Action::Direct, "LAN");
        lan.geoip = vec!["private".into()];
        let mut ru = rule(Action::Direct, "RU");
        ru.geoip = vec!["ru".into()];
        let mut whole = rule(Action::Direct, "RU целиком");
        whole.geoip = vec!["ru".into()];
        whole.geosite = vec!["category-ru".into()];
        let routing = Routing {
            fallback: Action::Proxy,
            geo_dir: None,
            rules: vec![sites, lan, ru, whole],
            ..Routing::default()
        };
        let r = Router::build(&routing, &dir);
        // A table whose caller resolves sites has no such warning to give.
        assert!(
            !Router::build_resolving(&routing, &dir)
                .warnings
                .iter()
                .any(|w| w.contains("сайты это правило не поймает"))
        );

        let all = r.warnings.join("\n");
        // One such warning, for `RU`: `private` means addresses, and the last
        // rule already carries its country's sites.
        let blind: Vec<&String> = r
            .warnings
            .iter()
            .filter(|w| w.contains("адреса, а браузер"))
            .collect();
        assert_eq!(blind.len(), 1, "{all}");
        assert!(
            blind[0].starts_with("правило #3 (RU): geoip:ru сравнивает адреса"),
            "{all}"
        );
        assert!(
            blind[0].ends_with("Их ловит geosite:category-ru — добавьте его в это же правило."),
            "{all}"
        );

        // Decided before any rule with a country: nothing to explain.
        assert_eq!(r.geoip_note(&Target::domain("yandex.ru", 443), None), None);
        // Past `RU` unseen, and caught by the category in the rule after it.
        let site = Target::domain("mail.ru", 443);
        assert_eq!(r.decide(&site, None).rule, "RU целиком");
        assert_eq!(r.geoip_passed_over(&site, None), ["RU"]);
        let note = r.geoip_note(&site, None).expect("a note for a name");
        assert!(note.contains("правило «RU»"), "{note}");
        // An address is what geoip does see, however it was spelled.
        let address = Target::Socket("1.2.3.4:443".parse().expect("an address"));
        assert!(r.geoip_passed_over(&address, None).is_empty());
        assert!(
            r.geoip_passed_over(&Target::domain("1.2.3.4", 443), None)
                .is_empty()
        );

        // Without geosite.dat the warning stands, with no category to name.
        let bare = Router::build(&routing, Path::new("nonexistent"));
        let all = bare.warnings.join("\n");
        assert!(all.contains("geoip:ru сравнивает адреса"), "{all}");
        assert!(all.contains("Их ловят условия geosite и домены"), "{all}");
    }
}
