//! Rule conditions as people type them: one to a line, in the words v2rayN and
//! xray use.
//!
//! The editor has one box for conditions rather than six lists, because that
//! is how anyone who has set up another client already writes a rule —
//! `geosite:google`, `geoip:ru`, `full:a.example`. The prefix says which of the
//! rule's lists a condition goes in; a value without one is read by its shape.

use std::net::IpAddr;

use bliss_proto::{Action, Rule};

/// Which of a rule's lists a condition belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum List {
    Domain,
    Geosite,
    Ip,
    Geoip,
    Port,
    Process,
}

/// Every prefix `condition` reads, the ones it refuses included.
const PREFIXES: [&str; 14] = [
    "geosite", "geoip", "domain", "suffix", "full", "keyword", "ip", "cidr", "port", "process",
    "app", "regexp", "regex", "ext",
];

/// A rule's conditions, one to a line, the way the editor shows them.
///
/// Geo categories, ports and programs keep their prefix, so a bare value never
/// has to be guessed at and `parse` reads the text back into the same rule.
pub fn to_text(rule: &Rule) -> String {
    let prefixed = |prefix: &str, list: &[String]| -> Vec<String> {
        list.iter()
            .map(|value| format!("{prefix}:{value}"))
            .collect()
    };
    let mut lines = rule.domain.clone();
    lines.extend(prefixed("geosite", &rule.geosite));
    lines.extend(rule.ip.iter().cloned());
    lines.extend(prefixed("geoip", &rule.geoip));
    lines.extend(prefixed("port", &rule.port));
    lines.extend(prefixed("process", &rule.process));
    lines.join("\n")
}

/// Read the editor into a rule.
///
/// One condition that does not read refuses the whole rule and says which one
/// it was: a rule saved without it would route traffic nobody asked about.
pub fn parse(name: &str, action: Action, text: &str) -> Result<Rule, String> {
    let name = name.trim();
    let mut rule = Rule {
        name: (!name.is_empty()).then(|| name.to_owned()),
        action,
        domain: Vec::new(),
        ip: Vec::new(),
        port: Vec::new(),
        process: Vec::new(),
        geosite: Vec::new(),
        geoip: Vec::new(),
    };
    for entry in text
        .split(['\n', ',', ';'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (list, value) =
            condition(entry).map_err(|why| format!("«{}»: {why}", shorten(entry)))?;
        let list = match list {
            List::Domain => &mut rule.domain,
            List::Geosite => &mut rule.geosite,
            List::Ip => &mut rule.ip,
            List::Geoip => &mut rule.geoip,
            List::Port => &mut rule.port,
            List::Process => &mut rule.process,
        };
        if !list.contains(&value) {
            list.push(value);
        }
    }
    if rule.is_empty() {
        return Err(
            "нужно хотя бы одно условие: домен, набор сайтов, адрес, порт или программа".to_owned(),
        );
    }
    Ok(rule)
}

/// One condition: the list it goes in, and its value as the profile keeps it.
pub fn condition(entry: &str) -> Result<(List, String), String> {
    let entry = entry.trim();
    // The address of a page, pasted whole: the site is what was meant.
    if entry.contains("://") {
        return from_url(entry);
    }
    // A whole path, copied from Explorer. Its drive letter is not a prefix.
    if let [drive, b':', b'\\' | b'/', ..] = entry.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        return program(entry).map(|v| (List::Process, v));
    }
    if let Some((prefix, value)) = entry.split_once(':') {
        let value = value.trim();
        match prefix.trim().to_ascii_lowercase().as_str() {
            "geosite" => return category(value).map(|v| (List::Geosite, v)),
            "geoip" => return category(value).map(|v| (List::Geoip, v)),
            "domain" | "suffix" => return host(value).map(|v| (List::Domain, v)),
            "full" => return host(value).map(|v| (List::Domain, format!("full:{v}"))),
            "keyword" => return keyword(value).map(|v| (List::Domain, format!("keyword:{v}"))),
            "ip" | "cidr" => return address(value).map(|v| (List::Ip, v)),
            "port" => return port(value).map(|v| (List::Port, v)),
            "process" | "app" => return program(value).map(|v| (List::Process, v)),
            "regexp" | "regex" => {
                return Err("регулярные выражения bliss пока не понимает".to_owned());
            }
            "ext" => return Err("свои файлы наборов (ext:) bliss пока не понимает".to_owned()),
            other
                if !other.is_empty()
                    && other.chars().all(|c| c.is_ascii_alphabetic())
                    && !value.chars().all(|c| c.is_ascii_digit()) =>
            {
                return Err(format!(
                    "не знаю приставки {other}: — есть domain, full, keyword, geosite, geoip, \
                     ip, port и process"
                ));
            }
            // Not a prefix: an IPv6 address, or a name with a port after it.
            _ => {}
        }
    }
    match address(entry) {
        Ok(value) => return Ok((List::Ip, value)),
        // An address with a mask that does not fit: that is the thing to say,
        // not that it is no domain.
        Err(why)
            if entry
                .split_once('/')
                .is_some_and(|(ip, _)| ip.trim().parse::<IpAddr>().is_ok()) =>
        {
            return Err(why);
        }
        Err(_) => {}
    }
    if entry.chars().all(|c| c.is_ascii_digit() || c == '-') {
        return port(entry).map(|v| (List::Port, v));
    }
    if entry.to_ascii_lowercase().ends_with(".exe") {
        return program(entry).map(|v| (List::Process, v));
    }
    if let Some((name, number)) = entry.rsplit_once(':')
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
    {
        // A rule matches when any one condition does, so `name:port` cannot
        // mean "this port of this site" — it would be two separate conditions.
        return Err(format!(
            "порт в правиле — отдельное условие: {} или port:{number}",
            shorten(name)
        ));
    }
    // `*.example.com` is how other programs write a wildcard; a domain here
    // already covers everything under it.
    host(entry.strip_prefix("*.").unwrap_or(entry)).map(|v| (List::Domain, v))
}

/// Whether text typed into "check an address" is a rule condition instead,
/// like `geoip:ru` — something to add to a rule rather than something to check.
pub fn looks_like_condition(text: &str) -> bool {
    text.trim()
        .split_once(':')
        .is_some_and(|(prefix, _)| PREFIXES.contains(&prefix.trim().to_ascii_lowercase().as_str()))
}

/// The site a host belongs to, for a rule meant to cover all of it:
/// `rr3---sn-4g5e.googlevideo.com` gives `googlevideo.com`.
///
/// Two labels, or three under a short second level such as `co.uk` or
/// `com.br`, so the guess errs narrow: a rule for `bbc.co.uk` is what was
/// meant, and one for `co.uk` would take a country's shops with it. `None` for
/// an address, which has no site.
pub fn site(host: &str) -> Option<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() || bare.parse::<IpAddr>().is_ok() {
        return None;
    }
    let labels: Vec<&str> = host.split('.').collect();
    let count = labels.len();
    if count <= 2 {
        return Some(host);
    }
    let keep = if labels[count - 1].len() == 2 && labels[count - 2].len() <= 3 {
        3
    } else {
        2
    };
    Some(labels[count - keep..].join("."))
}

fn category(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Err("не указано имя набора".to_owned());
    }
    if value.starts_with('!') {
        return Err("исключения (!) bliss пока не понимает".to_owned());
    }
    if value.contains('@') {
        return Err("атрибуты наборов (@…) bliss пока не понимает".to_owned());
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        Ok(value)
    } else {
        Err("в имени набора бывают только латиница, цифры и дефис".to_owned())
    }
}

fn host(value: &str) -> Result<String, String> {
    const NOT_A_DOMAIN: &str = "это не похоже на домен";
    let value = value.trim().trim_end_matches('.');
    if value.is_empty() {
        return Err("не указан домен".to_owned());
    }
    // An international name is compared in the ASCII form programs send.
    let value = if value.is_ascii() {
        value.to_ascii_lowercase()
    } else {
        reqwest::Url::parse(&format!("http://{value}/"))
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .ok_or_else(|| NOT_A_DOMAIN.to_owned())?
    };
    let fine = value.split('.').all(|label| {
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    if fine {
        Ok(value)
    } else {
        Err(NOT_A_DOMAIN.to_owned())
    }
}

fn keyword(value: &str) -> Result<String, String> {
    let value = value.trim().to_ascii_lowercase();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        return Err("после keyword: нужно слово без пробелов".to_owned());
    }
    Ok(value)
}

fn address(value: &str) -> Result<String, String> {
    let value = value.trim();
    let (ip, bits) = match value.split_once('/') {
        Some((ip, bits)) => (ip.trim(), Some(bits.trim())),
        None => (value, None),
    };
    let parsed: IpAddr = ip
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .map_err(|_| "это не адрес и не подсеть".to_owned())?;
    let width = if parsed.is_ipv4() { 32 } else { 128 };
    match bits {
        None => Ok(parsed.to_string()),
        Some(bits) => match bits.parse::<u8>() {
            Ok(bits) if bits <= width => Ok(format!("{parsed}/{bits}")),
            _ => Err(format!("после / нужна длина маски, от 0 до {width}")),
        },
    }
}

fn port(value: &str) -> Result<String, String> {
    let wrong = || "порт — число от 1 до 65535 или диапазон вроде 6881-6889".to_owned();
    let number = |text: &str| text.trim().parse::<u16>().ok().filter(|&port| port > 0);
    match value.trim().split_once('-') {
        Some((low, high)) => {
            let low = number(low).ok_or_else(wrong)?;
            let high = number(high).ok_or_else(wrong)?;
            // Written backwards is a typo, and the router reads it the same way.
            Ok(format!("{}-{}", low.min(high), low.max(high)))
        }
        None => number(value).map(|port| port.to_string()).ok_or_else(wrong),
    }
}

fn program(value: &str) -> Result<String, String> {
    // A whole path, copied from Explorer: the program is its last part.
    let name = value
        .trim()
        .trim_matches('"')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .trim();
    if name.is_empty() {
        return Err("не указана программа".to_owned());
    }
    // Windows names a program with its extension, so a rule without one would
    // never match anything.
    Ok(if name.contains('.') {
        name.to_owned()
    } else {
        format!("{name}.exe")
    })
}

fn from_url(entry: &str) -> Result<(List, String), String> {
    let url = reqwest::Url::parse(entry).map_err(|_| "ссылка не читается".to_owned())?;
    let host = url
        .host_str()
        .ok_or_else(|| "в ссылке нет адреса сайта".to_owned())?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    match bare.parse::<IpAddr>() {
        Ok(ip) => Ok((List::Ip, ip.to_string())),
        Err(_) => self::host(bare).map(|v| (List::Domain, v)),
    }
}

fn shorten(text: &str) -> String {
    const WIDTH: usize = 40;
    if text.chars().count() <= WIDTH {
        return text.to_owned();
    }
    let keep: String = text.chars().take(WIDTH - 1).collect();
    format!("{keep}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Rule {
        parse("", Action::Direct, text).unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    /// The report that started this: `geoip:ru` typed into bliss and nowhere
    /// to put it.
    #[test]
    fn prefixed_conditions_go_in_their_lists() {
        let rule = read(
            "geoip:ru\ngeosite:category-ru\nfull:a.example\nkeyword:video\ndomain:b.example\n\
             ip:10.0.0.0/8\nport:443\nprocess:qbittorrent.exe",
        );
        assert_eq!(rule.geoip, ["ru"]);
        assert_eq!(rule.geosite, ["category-ru"]);
        assert_eq!(
            rule.domain,
            ["full:a.example", "keyword:video", "b.example"]
        );
        assert_eq!(rule.ip, ["10.0.0.0/8"]);
        assert_eq!(rule.port, ["443"]);
        assert_eq!(rule.process, ["qbittorrent.exe"]);
        assert_eq!(rule.name, None, "an empty name is no name");
    }

    #[test]
    fn a_bare_value_is_read_by_its_shape() {
        let rule = read(
            "YouTube.com, 203.0.113.0/24; 2001:db8::/32\n6881-6889\n25\nTelegram.exe\n\
             *.c.example\nhttps://www.d.example/watch?v=1",
        );
        assert_eq!(rule.domain, ["youtube.com", "c.example", "www.d.example"]);
        assert_eq!(rule.ip, ["203.0.113.0/24", "2001:db8::/32"]);
        assert_eq!(rule.port, ["6881-6889", "25"]);
        assert_eq!(rule.process, ["Telegram.exe"]);
    }

    #[test]
    fn the_editor_text_reads_back_into_the_same_rule() {
        let rule = read(
            "full:a.example\nb.example\ngeosite:google\n1.2.3.4\ngeoip:private\n\
             port:80-81\nprocess:app.exe",
        );
        let again = parse("", Action::Direct, &to_text(&rule)).expect("reads back");
        assert_eq!(again, rule);
    }

    #[test]
    fn what_cannot_work_is_refused_with_the_reason() {
        for (text, expect) in [
            ("regexp:^ads", "регулярные"),
            ("a.example:443", "port:443"),
            ("10.0.0.0/33", "маски"),
            ("port:0", "от 1 до 65535"),
            ("geosite:!cn", "исключения"),
            ("geosite:category-ads-all@ads", "атрибуты"),
            ("foo:bar", "приставки foo"),
            ("not a domain", "не похоже на домен"),
            ("", "хотя бы одно условие"),
            (" , ;\n", "хотя бы одно условие"),
        ] {
            let why = parse("", Action::Proxy, text).expect_err(text);
            assert!(why.contains(expect), "{text:?}: {why}");
        }
    }

    #[test]
    fn programs_and_names_are_taken_the_way_they_arrive() {
        let path = format!(
            "C:{b}Program Files{b}Some App{b}Some App.exe",
            b = char::from(92)
        );
        assert_eq!(
            condition(&path),
            Ok((List::Process, "Some App.exe".to_owned()))
        );
        assert_eq!(
            condition("process:telegram"),
            Ok((List::Process, "telegram.exe".to_owned()))
        );
        // Programs send the ASCII form of an international name.
        assert_eq!(
            condition("пример.рф"),
            Ok((List::Domain, "xn--e1afmkfd.xn--p1ai".to_owned()))
        );
    }

    #[test]
    fn repeated_conditions_are_kept_once() {
        let rule = read("a.example\nA.example\ngeoip:ru, geoip:RU");
        assert_eq!(rule.domain, ["a.example"]);
        assert_eq!(rule.geoip, ["ru"]);
    }

    #[test]
    fn conditions_are_told_apart_from_addresses_to_check() {
        assert!(looks_like_condition("geoip:ru"));
        assert!(looks_like_condition(" GeoSite:google"));
        assert!(!looks_like_condition("youtube.com"));
        assert!(!looks_like_condition("youtube.com:443"));
        assert!(!looks_like_condition("2001:db8::1"));
    }

    #[test]
    fn a_site_is_guessed_narrow() {
        for (host, expect) in [
            ("rr3---sn-4g5edn6r.googlevideo.com", Some("googlevideo.com")),
            ("news.bbc.co.uk", Some("bbc.co.uk")),
            ("mail.yandex.ru", Some("yandex.ru")),
            ("www.google.com.br", Some("google.com.br")),
            ("t.me", Some("t.me")),
            ("localhost", Some("localhost")),
            ("203.0.113.7", None),
            ("[2001:db8::1]", None),
        ] {
            assert_eq!(site(host).as_deref(), expect, "{host}");
        }
    }
}
