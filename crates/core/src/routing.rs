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
}

impl Compiled {
    fn matches(&self, host: &str, ip: Option<IpAddr>, port: u16, process: Option<&str>) -> bool {
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
        if let Some(process) = process {
            let process = process.to_ascii_lowercase();
            if self.processes.contains(&process) {
                return true;
            }
        }
        false
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
        let mut warnings = Vec::new();

        // Load every category any rule mentions, once, rather than per rule.
        let wanted_sites: Vec<String> = config
            .rules
            .iter()
            .flat_map(|r| r.geosite.iter().cloned())
            .collect();
        let wanted_ips: Vec<String> = config
            .rules
            .iter()
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

        let mut rules = Vec::with_capacity(config.rules.len());
        for (index, rule) in config.rules.iter().enumerate() {
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
                processes: rule
                    .process
                    .iter()
                    .map(|p| p.trim().to_ascii_lowercase())
                    .collect(),
                sites: Vec::new(),
                ips: Vec::new(),
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
                                "{where_}: в наборе {raw} есть {} шаблон(ов) вида regex, \n                                 их bliss пока не умеет",
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
            Target::Domain { host, .. } => host.parse().ok(),
        };
        let port = target.port();

        for rule in &self.rules {
            if rule.matches(&host, ip, port, process) {
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

    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }

    pub fn fallback(&self) -> Route {
        self.fallback
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
                "не удалось прочитать {} ({e}) — правила, которые его используют, \n                 не будут срабатывать",
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
            domain: Vec::new(),
            ip: Vec::new(),
            port: Vec::new(),
            process: Vec::new(),
            geosite: Vec::new(),
            geoip: Vec::new(),
        }
    }

    fn router(rules: Vec<Rule>, fallback: Action) -> Router {
        Router::build(
            &Routing {
                fallback,
                geo_dir: None,
                rules,
            },
            Path::new("nonexistent"),
        )
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

    /// A geoip rule cannot match a name, and the user has to be able to find
    /// that out from the warnings rather than from their traffic.
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
}
