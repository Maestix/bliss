//! Subscriptions: a provider's server list, fetched from an address.
//!
//! What comes back is nearly everywhere the same thing: share links, one to a
//! line, usually base64-encoded as a whole. Headers may add the traffic used
//! and the expiry (`subscription-userinfo`) and a title (`profile-title`).
//! Panels pick the format by user agent, and a client they do not recognise
//! gets the plain links, which is what bliss wants — so it introduces itself as
//! itself rather than as another client.

use std::net::SocketAddr;
use std::time::Duration;

use base64::Engine as _;
use bliss_proto::{ServerConfig, Usage};
use futures_util::StreamExt;

/// The most a list may weigh. A thousand links is well under a megabyte. The
/// body is read up to this and no further, so a wrong address serving a large
/// file costs four megabytes rather than whatever the file is.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

/// The most servers one list may bring.
pub const MAX_SERVERS: usize = 2_000;

/// A list older than this is fetched again: at startup, and on the hour.
pub const REFRESH_AFTER: Duration = Duration::from_secs(12 * 3600);

const USER_AGENT: &str = concat!("bliss/", env!("CARGO_PKG_VERSION"));

/// The server links in a text.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Links {
    /// vless:// links that parse, in order, each once.
    pub links: Vec<String>,
    /// Links to protocols bliss does not speak: vmess://, trojan://, ss://.
    pub foreign: usize,
    /// vless:// links that do not parse.
    pub broken: usize,
}

/// The links in a fetched list or a pasted text, base64-encoded or not.
pub fn links_in(text: &str) -> Links {
    let decoded = decode(text);
    let text = decoded.as_deref().unwrap_or(text);
    let mut found = Links::default();
    for line in text.lines().map(str::trim) {
        let Some((scheme, _)) = line.split_once("://") else {
            continue;
        };
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "vless" {
            // The address of a page is not a server in another protocol.
            if !scheme.is_empty() && scheme != "http" && scheme != "https" {
                found.foreign += 1;
            }
            continue;
        }
        if line.parse::<ServerConfig>().is_err() {
            found.broken += 1;
            continue;
        }
        if found.links.len() == MAX_SERVERS {
            break;
        }
        if !found.links.iter().any(|known| known == line) {
            found.links.push(line.to_owned());
        }
    }
    found
}

/// The text inside a base64-encoded list, or `None` when it is not one.
fn decode(text: &str) -> Option<String> {
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() || compact.contains("://") {
        return None;
    }
    [STANDARD, URL_SAFE, STANDARD_NO_PAD, URL_SAFE_NO_PAD]
        .iter()
        .find_map(|engine| engine.decode(&compact).ok())
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// `subscription-userinfo: upload=455727941; download=6174315083; total=0; expire=1735689600`
pub fn usage(header: &str) -> Option<Usage> {
    let mut usage = Usage::default();
    let mut any = false;
    for part in header.split(';') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let value = value.trim();
        // Some panels write floats. And a profile is TOML, which cannot hold an
        // integer past i64::MAX, so nothing larger is let in to fail the save.
        let Some(number) = value.parse::<u64>().ok().or_else(|| {
            value
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .map(|n| n as u64)
        }) else {
            continue;
        };
        let number = number.min(i64::MAX as u64);
        match key.trim().to_ascii_lowercase().as_str() {
            "upload" => usage.upload = number,
            "download" => usage.download = number,
            "total" => usage.total = number,
            "expire" => usage.expire = (number > 0).then_some(number as i64),
            _ => continue,
        }
        any = true;
    }
    any.then_some(usage)
}

/// `profile-title`: the text itself, or `base64:` and the text encoded.
pub fn title(header: &str) -> Option<String> {
    use base64::engine::general_purpose::STANDARD;
    let header = header.trim();
    let text = match header.strip_prefix("base64:") {
        Some(encoded) => String::from_utf8(STANDARD.decode(encoded.trim()).ok()?).ok()?,
        None => header.to_owned(),
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(60).collect())
}

/// What a fetch brought back.
#[derive(Debug)]
pub struct Fetched {
    pub links: Links,
    pub title: Option<String>,
    pub usage: Option<Usage>,
}

/// Fetch a list, through a SOCKS5 address when `via` is given.
///
/// Errors are for the window, and never contain the address: it carries the
/// token, and reqwest writes the URL into its own messages, which is why those
/// are described by kind here rather than shown.
pub async fn fetch(url: &str, via: Option<SocketAddr>) -> Result<Fetched, String> {
    let builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(40));
    let builder = match via {
        Some(addr) => builder.proxy(
            reqwest::Proxy::all(format!("socks5h://{addr}"))
                .map_err(|_| "не удалось направить загрузку через туннель".to_owned())?,
        ),
        // Straight means straight. HTTP_PROXY on this machine can point at a
        // client that is not running, or at bliss itself.
        None => builder.no_proxy(),
    };
    let client = builder
        .build()
        .map_err(|_| "не удалось подготовить загрузку".to_owned())?;

    let response = client.get(url).send().await.map_err(describe)?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("сервер подписки ответил кодом {}", status.as_u16()));
    }
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
    };
    let usage = header("subscription-userinfo").as_deref().and_then(usage);
    let title = header("profile-title").as_deref().and_then(title);
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(too_large());
    }

    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(describe)?;
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body);
    let links = links_in(&text);
    if links.links.is_empty() {
        return Err(nothing_usable(&text, &links));
    }
    Ok(Fetched {
        links,
        title,
        usage,
    })
}

/// Why a list that arrived holds no server bliss can use.
fn nothing_usable(text: &str, links: &Links) -> String {
    let start = text.trim_start().get(..200).unwrap_or(text.trim_start());
    let start = start.to_ascii_lowercase();
    if start.starts_with("<!doctype") || start.starts_with("<html") {
        return "по этому адресу страница для браузера, а не список серверов".to_owned();
    }
    if start.starts_with("proxies:") || start.contains("\nproxies:") {
        return "это список для Clash, а bliss нужен список ссылок".to_owned();
    }
    if links.foreign > 0 {
        return format!(
            "в списке {} других протоколов и ни одной ссылки vless://",
            crate::ui::plural(links.foreign as u64, "сервер", "сервера", "серверов")
        );
    }
    if links.broken > 0 {
        return "ссылки vless:// в списке не читаются".to_owned();
    }
    "в ответе нет ссылок vless://".to_owned()
}

fn too_large() -> String {
    format!(
        "ответ больше {} МБ — это не похоже на список серверов",
        MAX_BODY / (1024 * 1024)
    )
}

fn describe(error: reqwest::Error) -> String {
    // The kind only, in the log as on screen; see `fetch`.
    let error = error.without_url();
    tracing::info!(
        "subscription: fetch failed (timeout {}, connect {}, body {})",
        error.is_timeout(),
        error.is_connect(),
        error.is_body()
    );
    if error.is_timeout() {
        "сервер подписки не ответил вовремя".to_owned()
    } else if error.is_connect() {
        "не удалось подключиться к серверу подписки".to_owned()
    } else if error.is_body() || error.is_decode() {
        "список оборвался на середине".to_owned()
    } else {
        "не удалось загрузить список".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str =
        "vless://11111111-2222-3333-4444-555555555555@a.example:443?encryption=none#one";
    const TWO: &str =
        "vless://11111111-2222-3333-4444-555555555555@b.example:443?encryption=none#two";

    #[test]
    fn a_base64_list_and_a_plain_one_read_the_same() {
        use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
        let plain = format!("{ONE}\r\n{TWO}\nvmess://AAAA\ntrojan://x@c.example:443\n\n");
        let expect = Links {
            links: vec![ONE.to_owned(), TWO.to_owned()],
            foreign: 2,
            broken: 0,
        };
        assert_eq!(links_in(&plain), expect);
        assert_eq!(links_in(&STANDARD.encode(&plain)), expect);
        assert_eq!(links_in(&URL_SAFE_NO_PAD.encode(&plain)), expect);
        // Wrapped at 76 columns, as some panels send it.
        let wrapped: String = STANDARD
            .encode(&plain)
            .as_bytes()
            .chunks(76)
            .map(|line| String::from_utf8_lossy(line).into_owned() + "\n")
            .collect();
        assert_eq!(links_in(&wrapped), expect);
    }

    #[test]
    fn a_list_keeps_each_server_once_and_counts_what_it_skipped() {
        let text = format!("{ONE}\n{ONE}\nvless://broken\nhttps://sub.example/page");
        let found = links_in(&text);
        assert_eq!(found.links, [ONE]);
        assert_eq!(found.broken, 1);
        assert_eq!(found.foreign, 0, "a page address is not a server");
    }

    /// The cap is a cap on what is kept, whatever the list holds.
    #[test]
    fn a_list_brings_at_most_max_servers() {
        let text: String = (0..MAX_SERVERS + 50)
            .map(|n| {
                format!(
                    "vless://11111111-2222-3333-4444-555555555555@h{n}.example:443?encryption=none\n"
                )
            })
            .collect();
        assert_eq!(links_in(&text).links.len(), MAX_SERVERS);
    }

    #[test]
    fn usage_is_read_from_the_header_providers_send() {
        assert_eq!(
            usage("upload=455727941; download=6174315083; total=1073741824000; expire=1735689600"),
            Some(Usage {
                upload: 455_727_941,
                download: 6_174_315_083,
                total: 1_073_741_824_000,
                expire: Some(1_735_689_600),
            })
        );
        let odd = usage("upload=1.5e3;download=; total=99999999999999999999; expire=0")
            .expect("some of it reads");
        assert_eq!(odd.upload, 1500);
        assert_eq!(odd.total, i64::MAX as u64, "clamped to what TOML can hold");
        assert_eq!(odd.expire, None, "0 means it does not expire");
        assert_eq!(usage("nonsense"), None);
    }

    #[test]
    fn a_title_arrives_plain_or_encoded() {
        use base64::engine::general_purpose::STANDARD;
        assert_eq!(title(" Провайдер "), Some("Провайдер".to_owned()));
        assert_eq!(
            title(&format!("base64:{}", STANDARD.encode("Мой VPN"))),
            Some("Мой VPN".to_owned())
        );
        assert_eq!(title("base64:!!!"), None);
        assert_eq!(title("   "), None);
    }

    #[test]
    fn an_empty_answer_is_explained() {
        let none = Links::default();
        assert!(nothing_usable("<!DOCTYPE html><html>", &none).contains("браузера"));
        assert!(nothing_usable("proxies:\n  - name: a", &none).contains("Clash"));
        let foreign = Links {
            foreign: 3,
            ..Links::default()
        };
        assert!(nothing_usable("vmess://…", &foreign).contains("3 сервера"));
    }
}
