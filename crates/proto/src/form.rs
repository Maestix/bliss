//! A server as a form: every field a person can fill in by hand, as text.
//!
//! The profile keeps servers as share links (`ServerEntry::link`), so a server
//! added by hand is a link this module writes, and one being looked at or
//! edited is a parsed link read back into the form. Both directions go
//! through the very parser a pasted link does: `to_link` hands back only a
//! link that parses, so the window cannot save a server the engine would
//! refuse at start.
//!
//! Shared by the desktop window and the phone, which sends the form over its
//! socket as JSON; hence the plain strings.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::error::LinkError;
use crate::server::{Address, KcpHeader, Protocol, Security, ServerConfig, TcpHeader, Transport};

/// What a user part, a fragment or a query value may hold as it is.
const KEEP: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The protocols a form can be, by the key it is sent with.
pub const PROTOCOLS: [(&str, &str); 7] = [
    ("vless", "VLESS"),
    ("vmess", "VMess"),
    ("trojan", "Trojan"),
    ("ss", "Shadowsocks"),
    ("socks", "SOCKS"),
    ("hysteria2", "Hysteria2"),
    ("wireguard", "WireGuard"),
];

/// Shadowsocks' methods, for the form to offer; the parser holds the same list.
pub const SS_METHODS: [&str; 9] = [
    "2022-blake3-aes-128-gcm",
    "2022-blake3-aes-256-gcm",
    "2022-blake3-chacha20-poly1305",
    "aes-128-gcm",
    "aes-256-gcm",
    "chacha20-ietf-poly1305",
    "xchacha20-ietf-poly1305",
    "chacha20-poly1305",
    "none",
];

/// Every field, as text; a field the protocol does not use is ignored.
///
/// `Debug` shows the protocol and the address alone: the form holds the
/// server's secrets.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerForm {
    /// `vless`, `vmess`, `trojan`, `ss`, `socks`, `hysteria2`, `wireguard`.
    pub protocol: String,
    pub name: String,
    pub address: String,
    pub port: String,

    /// VLESS and VMess: the user's UUID.
    pub id: String,
    /// VLESS: `xtls-rprx-vision`, `xtls-rprx-vision-udp443` or empty.
    pub flow: String,
    /// VLESS: `none`, or a VLESS Encryption string.
    pub encryption: String,
    /// VMess: `auto`, `aes-128-gcm`, `chacha20-poly1305`, `none`, `zero`.
    pub cipher: String,
    /// Shadowsocks' method.
    pub method: String,
    /// SOCKS: the login's user name.
    pub user: String,
    /// Trojan, Shadowsocks and SOCKS: the password; Hysteria 2: its auth.
    pub password: String,

    /// `none`, `tls` or `reality`.
    pub security: String,
    pub sni: String,
    /// The browser the ClientHello is dressed as: `chrome`, `firefox`, …
    pub fingerprint: String,
    /// Comma-separated, as in a link.
    pub alpn: String,
    /// `pcs`: certificate SHA-256s, comma-separated.
    pub pinned: String,
    /// `vcn`: names to check the certificate against.
    pub verify_names: String,
    pub ech: String,
    /// REALITY: the server's public key, base64url.
    pub public_key: String,
    pub short_id: String,
    pub spider_x: String,
    /// REALITY: `pqv`, the server's ML-DSA-65 key.
    pub mldsa65: String,

    /// `tcp`, `ws`, `httpupgrade`, `xhttp`, `grpc` or `kcp`.
    pub transport: String,
    /// The HTTP host of ws, httpupgrade, xhttp and a tcp HTTP disguise.
    pub host: String,
    pub path: String,
    /// XHTTP: `auto`, `packet-up`, `stream-up`, `stream-one`.
    pub mode: String,
    /// XHTTP's `extra`, a JSON object.
    pub extra: String,
    /// gRPC.
    pub service_name: String,
    pub authority: String,
    pub multi: bool,
    /// tcp: `none` or `http`; kcp: `none`, `srtp`, `utp`, `wechat-video`, `dtls`, `wireguard`.
    pub header: String,
    /// mKCP.
    pub seed: String,
    pub mtu: String,

    /// Hysteria 2: Salamander's password.
    pub obfs: String,
    /// Hysteria 2: the ports to hop between, `20000-30000,443`.
    pub hop: String,

    /// WireGuard: this side's private key, base64.
    pub private_key: String,
    /// WireGuard: the server's public key, base64.
    pub peer_key: String,
    pub pre_shared_key: String,
    /// WireGuard: this side's addresses in the tunnel, comma-separated.
    pub local_addresses: String,
    /// WireGuard: three numbers, `1,2,3`.
    pub reserved: String,

    /// `fm`: xray's own `finalmask` object, carried as written.
    pub finalmask: String,
}

impl fmt::Debug for ServerForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerForm")
            .field("protocol", &self.protocol)
            .field("address", &self.address)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// One line of a server's details, for a screen.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct DetailRow {
    pub label: String,
    pub value: String,
    /// A key, a password or a UUID: drawn hidden until asked for.
    pub secret: bool,
}

impl fmt::Debug for DetailRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = if self.secret { "…" } else { &self.value };
        write!(f, "{}: {value}", self.label)
    }
}

impl ServerForm {
    /// An empty form for a new server: VLESS over REALITY, what most servers
    /// handed out today are.
    pub fn new_server() -> Self {
        Self {
            protocol: "vless".to_owned(),
            port: "443".to_owned(),
            encryption: "none".to_owned(),
            flow: "xtls-rprx-vision".to_owned(),
            security: "reality".to_owned(),
            fingerprint: "chrome".to_owned(),
            transport: "tcp".to_owned(),
            ..Self::default()
        }
    }

    /// The form a parsed server fills in, secrets included.
    pub fn from_config(cfg: &ServerConfig) -> Self {
        // A link without a name is named by where it points; that is no
        // name to write back into it.
        let unnamed = cfg.tag == format!("{}:{}", cfg.address, cfg.port);
        let mut form = Self {
            name: if unnamed {
                String::new()
            } else {
                cfg.tag.clone()
            },
            address: match &cfg.address {
                Address::Domain(d) => d.clone(),
                Address::Ip(ip) => ip.to_string(),
            },
            port: cfg.port.to_string(),
            finalmask: hidden(cfg.finalmask.as_ref()),
            ..Self::default()
        };
        match &cfg.protocol {
            Protocol::Vless {
                user,
                encryption,
                flow,
            } => {
                form.protocol = "vless".to_owned();
                form.id = user.0.to_string();
                form.encryption = encryption.as_param().to_owned();
                form.flow = flow.as_param().unwrap_or_default().to_owned();
            }
            Protocol::Vmess { user, cipher } => {
                form.protocol = "vmess".to_owned();
                form.id = user.0.to_string();
                form.cipher = cipher.as_param().to_owned();
            }
            Protocol::Trojan { password } => {
                form.protocol = "trojan".to_owned();
                form.password = password.0.clone();
            }
            Protocol::Shadowsocks { method, password } => {
                form.protocol = "ss".to_owned();
                form.method = method.clone();
                form.password = password.0.clone();
            }
            Protocol::Socks { login } => {
                form.protocol = "socks".to_owned();
                if let Some(login) = login {
                    form.user = login.user.0.clone();
                    form.password = login.pass.0.clone();
                }
            }
            Protocol::Hysteria2 { auth } => {
                form.protocol = "hysteria2".to_owned();
                form.password = auth.0.clone();
            }
            Protocol::WireGuard(wg) => {
                form.protocol = "wireguard".to_owned();
                form.private_key = STANDARD.encode(wg.secret_key.0);
                form.peer_key = STANDARD.encode(wg.peer_key.0);
                form.pre_shared_key = wg
                    .pre_shared_key
                    .as_ref()
                    .map(|k| STANDARD.encode(k.0))
                    .unwrap_or_default();
                form.local_addresses = wg.addresses.join(", ");
                form.reserved = wg
                    .reserved
                    .map(|r| format!("{},{},{}", r[0], r[1], r[2]))
                    .unwrap_or_default();
                form.mtu = wg.mtu.map(|m| m.to_string()).unwrap_or_default();
            }
        }
        match &cfg.security {
            Security::None => form.security = "none".to_owned(),
            Security::Tls(tls) => {
                form.security = "tls".to_owned();
                form.sni = tls.sni.clone().unwrap_or_default();
                form.fingerprint = tls.fingerprint.to_string();
                form.alpn = tls.alpn.join(",");
                form.pinned = tls.pinned.join(",");
                form.verify_names = tls.verify_names.join(",");
                form.ech = tls.ech.clone().unwrap_or_default();
            }
            Security::Reality(reality) => {
                form.security = "reality".to_owned();
                form.sni = reality.sni.clone();
                form.fingerprint = reality.fingerprint.to_string();
                form.public_key = URL_SAFE_NO_PAD.encode(reality.public_key.0);
                form.short_id = reality.short_id.to_string();
                form.spider_x = reality.spider_x.clone().unwrap_or_default();
                form.mldsa65 = reality
                    .mldsa65_verify
                    .as_ref()
                    .map(|l| l.0.clone())
                    .unwrap_or_default();
            }
        }
        match &cfg.transport {
            Transport::Tcp { header } => {
                form.transport = "tcp".to_owned();
                match header {
                    TcpHeader::None => form.header = "none".to_owned(),
                    TcpHeader::Http { host, path } => {
                        form.header = "http".to_owned();
                        form.host = host.join(",");
                        form.path = path.join(",");
                    }
                }
            }
            Transport::Ws { path, host } | Transport::HttpUpgrade { path, host } => {
                form.transport = cfg.transport.name().to_owned();
                form.path = path.clone();
                form.host = host.clone().unwrap_or_default();
            }
            Transport::Xhttp {
                path,
                host,
                mode,
                extra,
            } => {
                form.transport = "xhttp".to_owned();
                form.path = path.clone();
                form.host = host.clone().unwrap_or_default();
                form.mode = mode.clone();
                form.extra = hidden(extra.as_ref());
            }
            Transport::Grpc {
                service_name,
                authority,
                multi,
            } => {
                form.transport = "grpc".to_owned();
                form.service_name = service_name.clone();
                form.authority = authority.clone().unwrap_or_default();
                form.multi = *multi;
            }
            Transport::Kcp { header, seed, mtu } => {
                form.transport = "kcp".to_owned();
                form.header = match header {
                    KcpHeader::Wechat => "wechat-video".to_owned(),
                    other => other.name().to_owned(),
                };
                form.seed = hidden(seed.as_ref());
                form.mtu = mtu.map(|m| m.to_string()).unwrap_or_default();
            }
            Transport::Hysteria { obfs, hop } => {
                form.obfs = hidden(obfs.as_ref());
                form.hop = hop.clone().unwrap_or_default();
            }
            Transport::None => {}
        }
        form
    }

    /// The share link the form describes, checked by reading it back the
    /// way a pasted link is read. A refusal is in words for the window, and
    /// names a field rather than quoting a value.
    pub fn to_link(&self) -> Result<String, String> {
        let link = self.write_link()?;
        match link.parse::<ServerConfig>() {
            Ok(_) => Ok(link),
            Err(e) => Err(refusal(&e)),
        }
    }

    /// The link, unchecked.
    fn write_link(&self) -> Result<String, String> {
        let address = self.address.trim();
        if address.is_empty() {
            return Err("укажите адрес сервера".to_owned());
        }
        // Written as the parser will read it back — an IPv6 address in its
        // brackets, a name in IDNA's ASCII — so a saved server reads the
        // same in the form the next time it is opened.
        let host = Address::from_host(address)
            .map_err(|_| "в адресе сервера только имя или IP, без схемы и пути".to_owned())?
            .to_string();
        let port = match self.port.trim() {
            "" if self.protocol == "hysteria2" => 443,
            "" => return Err("укажите порт сервера".to_owned()),
            port => port
                .parse::<u16>()
                .ok()
                .filter(|p| *p > 0)
                .ok_or_else(|| "порт — число от 1 до 65535".to_owned())?,
        };
        let name = self.name.trim();
        let fragment = if name.is_empty() {
            String::new()
        } else {
            format!("#{}", enc(name))
        };
        let required = |value: &str, what: &str| -> Result<String, String> {
            let value = value.trim();
            if value.is_empty() {
                Err(format!("укажите {what}"))
            } else {
                Ok(value.to_owned())
            }
        };
        let mut q = Params::default();

        let user_part = match self.protocol.as_str() {
            "vless" => {
                let id = required(&self.id, "UUID пользователя")?;
                q.put("encryption", or(&self.encryption, "none"));
                q.put("flow", &self.flow);
                self.stream(&mut q)?;
                enc(&id)
            }
            "vmess" => {
                let id = required(&self.id, "UUID пользователя")?;
                q.put("encryption", or(&self.cipher, "auto"));
                self.stream(&mut q)?;
                enc(&id)
            }
            "trojan" => {
                let password = required(&self.password, "пароль")?;
                self.stream(&mut q)?;
                enc(&password)
            }
            "ss" => {
                let method = required(&self.method, "шифр")?;
                let password = required(&self.password, "пароль")?;
                self.stream(&mut q)?;
                URL_SAFE_NO_PAD.encode(format!("{method}:{password}"))
            }
            "socks" => {
                q.put("fm", &self.finalmask);
                match (self.user.trim(), self.password.as_str()) {
                    ("", "") => String::new(),
                    (user, pass) => format!("{}:{}", enc(user), enc(pass)),
                }
            }
            "hysteria2" => {
                let auth = required(&self.password, "пароль (auth)")?;
                q.put("sni", &self.sni);
                q.put("fp", &self.fingerprint);
                q.put("alpn", &self.alpn);
                q.put("pcs", &self.pinned);
                q.put("vcn", &self.verify_names);
                q.put("ech", &self.ech);
                if !self.obfs.trim().is_empty() {
                    q.put("obfs", "salamander");
                    q.put("obfs-password", &self.obfs);
                }
                q.put("mport", &self.hop);
                q.put("fm", &self.finalmask);
                enc(&auth)
            }
            "wireguard" => {
                let key = required(&self.private_key, "закрытый ключ")?;
                q.put(
                    "publickey",
                    &required(&self.peer_key, "открытый ключ сервера")?,
                );
                q.put("presharedkey", &self.pre_shared_key);
                q.put("address", &self.local_addresses.replace(' ', ""));
                q.put("reserved", &self.reserved.replace(' ', ""));
                q.put("mtu", &self.mtu);
                q.put("fm", &self.finalmask);
                enc(&key)
            }
            "" => return Err("выберите протокол".to_owned()),
            other => return Err(format!("протокола {other} bliss не знает")),
        };

        let at = if user_part.is_empty() {
            String::new()
        } else {
            format!("{user_part}@")
        };
        let query = q.finish();
        // Hysteria's own links have a slash before the query; the rest do not.
        let slash = if self.protocol == "hysteria2" {
            "/"
        } else {
            ""
        };
        Ok(format!(
            "{}://{at}{host}:{port}{slash}{query}{fragment}",
            self.protocol
        ))
    }

    /// The security and the transport, as `link::stream` reads them.
    fn stream(&self, q: &mut Params) -> Result<(), String> {
        let security = or(&self.security, "none");
        q.put("security", security);
        match security {
            "none" => {}
            "tls" => {
                q.put("sni", &self.sni);
                q.put("fp", &self.fingerprint);
                q.put("alpn", &self.alpn);
                q.put("pcs", &self.pinned);
                q.put("vcn", &self.verify_names);
                q.put("ech", &self.ech);
            }
            "reality" => {
                if self.sni.trim().is_empty() {
                    return Err(
                        "для REALITY укажите SNI — имя сайта, под который он маскируется"
                            .to_owned(),
                    );
                }
                if self.public_key.trim().is_empty() {
                    return Err("для REALITY укажите открытый ключ сервера (pbk)".to_owned());
                }
                q.put("sni", &self.sni);
                q.put("fp", &self.fingerprint);
                q.put("pbk", &self.public_key);
                q.put("sid", &self.short_id);
                q.put("spx", &self.spider_x);
                q.put("pqv", &self.mldsa65);
            }
            other => return Err(format!("защиты {other} нет: none, tls или reality")),
        }
        let transport = or(&self.transport, "tcp");
        q.put("type", transport);
        match transport {
            "tcp" | "raw" => {
                if self.header.trim() == "http" {
                    q.put("headerType", "http");
                    q.put("host", &self.host);
                    q.put("path", &self.path);
                }
            }
            "ws" | "httpupgrade" => {
                q.put("host", &self.host);
                q.put("path", &self.path);
            }
            "xhttp" => {
                q.put("host", &self.host);
                q.put("path", &self.path);
                q.put("mode", &self.mode);
                q.put("extra", &self.extra);
            }
            "grpc" => {
                q.put("serviceName", &self.service_name);
                q.put("authority", &self.authority);
                if self.multi {
                    q.put("mode", "multi");
                }
            }
            "kcp" => {
                if !matches!(self.header.trim(), "" | "none") {
                    q.put("headerType", &self.header);
                }
                q.put("seed", &self.seed);
                q.put("mtu", &self.mtu);
            }
            other => return Err(format!("транспорта {other} нет в списке")),
        }
        q.put("fm", &self.finalmask);
        Ok(())
    }

    /// The server as lines for a screen: what it speaks, where, how it hides
    /// and what carries it — every field it has, secrets marked.
    pub fn rows(&self) -> Vec<DetailRow> {
        let mut rows = Vec::new();
        let mut put = |label: &str, value: &str, secret: bool| {
            let value = value.trim();
            if !value.is_empty() {
                rows.push(DetailRow {
                    label: label.to_owned(),
                    value: value.to_owned(),
                    secret,
                });
            }
        };
        let protocol = PROTOCOLS
            .iter()
            .find(|(key, _)| *key == self.protocol)
            .map_or(self.protocol.as_str(), |(_, name)| name);
        put("Протокол", protocol, false);
        put("Адрес", &self.address, false);
        put("Порт", &self.port, false);
        match self.protocol.as_str() {
            "vless" => {
                put("UUID", &self.id, true);
                put("Flow", &self.flow, false);
                put("Шифрование", &self.encryption, self.encryption != "none");
            }
            "vmess" => {
                put("UUID", &self.id, true);
                put("Шифр", &self.cipher, false);
            }
            "trojan" => put("Пароль", &self.password, true),
            "ss" => {
                put("Шифр", &self.method, false);
                put("Пароль", &self.password, true);
            }
            "socks" => {
                put("Пользователь", &self.user, true);
                put("Пароль", &self.password, true);
            }
            "hysteria2" => {
                put("Пароль (auth)", &self.password, true);
                put("Salamander", &self.obfs, true);
                put("Прыжки по портам", &self.hop, false);
            }
            "wireguard" => {
                put("Закрытый ключ", &self.private_key, true);
                put("Ключ сервера", &self.peer_key, false);
                put("Общий ключ (PSK)", &self.pre_shared_key, true);
                put("Адреса в туннеле", &self.local_addresses, false);
                put("Reserved", &self.reserved, false);
                put("MTU", &self.mtu, false);
            }
            _ => {}
        }
        if !matches!(self.protocol.as_str(), "wireguard" | "socks" | "hysteria2") {
            put("Защита", &self.security.to_uppercase(), false);
        }
        if self.security != "none" || self.protocol == "hysteria2" {
            put("SNI", &self.sni, false);
            put("Отпечаток", &self.fingerprint, false);
            put("ALPN", &self.alpn, false);
            put("Ключ REALITY (pbk)", &self.public_key, false);
            put("Short ID", &self.short_id, false);
            put("SpiderX", &self.spider_x, false);
            put("ML-DSA-65 (pqv)", &self.mldsa65, false);
            put("Закреплённый сертификат", &self.pinned, false);
            put("Имена сертификата", &self.verify_names, false);
            put("ECH", &self.ech, false);
        }
        if !matches!(self.protocol.as_str(), "wireguard" | "socks" | "hysteria2") {
            put("Транспорт", &self.transport, false);
            match self.transport.as_str() {
                "grpc" => {
                    put("serviceName", &self.service_name, false);
                    put("authority", &self.authority, false);
                    if self.multi {
                        put("Режим", "multi", false);
                    }
                }
                "kcp" => {
                    if self.header != "none" {
                        put("Маскировка", &self.header, false);
                    }
                    put("Seed", &self.seed, true);
                    put("MTU", &self.mtu, false);
                }
                _ => {
                    if self.transport == "tcp" && self.header == "http" {
                        put("Маскировка", "HTTP", false);
                    }
                    put("Host", &self.host, false);
                    put("Путь", &self.path, false);
                    put("Режим", &self.mode, false);
                    put("extra", &self.extra, true);
                }
            }
        }
        put("finalmask (fm)", &self.finalmask, true);
        rows
    }
}

/// The query, in the order written, empty values left out.
#[derive(Default)]
struct Params(Vec<(String, String)>);

impl Params {
    fn put(&mut self, key: &str, value: &str) {
        let value = value.trim();
        if !value.is_empty() {
            self.0.push((key.to_owned(), value.to_owned()));
        }
    }

    fn finish(self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        let pairs: Vec<String> = self
            .0
            .iter()
            .map(|(k, v)| format!("{k}={}", enc(v)))
            .collect();
        format!("?{}", pairs.join("&"))
    }
}

fn enc(text: &str) -> String {
    utf8_percent_encode(text, KEEP).to_string()
}

fn or<'a>(value: &'a str, default: &'a str) -> &'a str {
    match value.trim() {
        "" => default,
        value => value,
    }
}

fn hidden(value: Option<&crate::server::Hidden>) -> String {
    value.map(|h| h.0.clone()).unwrap_or_default()
}

/// Why the written link did not read back, in words; never the value.
fn refusal(e: &LinkError) -> String {
    fn field(param: &str) -> &str {
        match param {
            "flow" => "Flow",
            "encryption" => "шифрование",
            "security" => "защита",
            "pcs" => "закреплённый сертификат",
            "pbk" => "ключ REALITY",
            "sid" => "Short ID",
            "type" => "транспорт",
            "mode" => "режим",
            "headerType" => "маскировка",
            "mtu" => "MTU",
            "extra" => "extra",
            "fm" => "finalmask",
            "mport" => "прыжки по портам",
            "reserved" => "reserved",
            "private key" => "закрытый ключ",
            "publickey" => "ключ сервера",
            "presharedkey" => "общий ключ",
            other => other,
        }
    }
    match e {
        LinkError::MissingUuid | LinkError::Uuid(_) => {
            "UUID пользователя — 32 шестнадцатеричные цифры с дефисами".to_owned()
        }
        LinkError::MissingHost => "укажите адрес сервера".to_owned(),
        LinkError::MissingPort => "укажите порт сервера".to_owned(),
        LinkError::Url(_) => "адрес сервера не похож на имя или IP".to_owned(),
        LinkError::Missing(what) => format!("не заполнено: {}", field(what)),
        LinkError::RealityMissing(param) => format!("для REALITY не заполнено: {}", field(param)),
        LinkError::BadValue {
            param, expected, ..
        } => format!("поле «{}» неверно: ожидается {expected}", field(param)),
        LinkError::UnsupportedTransport(t) => format!("транспорта {t} в xray-core больше нет"),
        LinkError::Unsupported(what) => format!("xray-core так не умеет: {what}"),
        other => format!("сервер не собрался: {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "11111111-2222-4333-8444-555555555555";
    /// 32 bytes of 0x41, base64url without padding.
    const PBK: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
    /// 32 bytes, standard base64, as WireGuard writes keys.
    const WG_KEY: &str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE=";

    /// Written, read back, written again: the same form, so an edit that
    /// changes nothing changes nothing.
    fn round_trip(form: &ServerForm) -> ServerForm {
        let link = form.to_link().unwrap_or_else(|e| panic!("{form:?}: {e}"));
        let cfg: ServerConfig = link.parse().unwrap();
        let back = ServerForm::from_config(&cfg);
        // What a link leaves to defaults (the fingerprint) is written out
        // once read; from there on the link does not move.
        let again = back.to_link().unwrap();
        let cfg: ServerConfig = again.parse().unwrap();
        assert_eq!(
            ServerForm::from_config(&cfg).to_link().unwrap(),
            again,
            "the link is stable"
        );
        back
    }

    #[test]
    fn a_vless_reality_server_is_written_and_read_back() {
        let form = ServerForm {
            name: "Нидерланды 🇳🇱".to_owned(),
            address: "a.example".to_owned(),
            id: UUID.to_owned(),
            sni: "www.example.net".to_owned(),
            public_key: PBK.to_owned(),
            short_id: "0123abcd".to_owned(),
            ..ServerForm::new_server()
        };
        let back = round_trip(&form);
        assert_eq!(back.name, form.name);
        assert_eq!(back.id, UUID);
        assert_eq!(back.flow, "xtls-rprx-vision");
        assert_eq!(back.security, "reality");
        assert_eq!(back.public_key, PBK);
        assert_eq!(back.short_id, "0123abcd");
        assert_eq!(back.port, "443");
    }

    #[test]
    fn every_protocol_round_trips() {
        let forms = [
            ServerForm {
                protocol: "vmess".to_owned(),
                address: "203.0.113.5".to_owned(),
                port: "8080".to_owned(),
                id: UUID.to_owned(),
                cipher: "aes-128-gcm".to_owned(),
                security: "tls".to_owned(),
                sni: "a.example".to_owned(),
                alpn: "h2,http/1.1".to_owned(),
                transport: "ws".to_owned(),
                host: "a.example".to_owned(),
                path: "/ws?ed=2048".to_owned(),
                ..ServerForm::default()
            },
            ServerForm {
                protocol: "trojan".to_owned(),
                address: "a.example".to_owned(),
                port: "443".to_owned(),
                password: "PP:PP@P#P".to_owned(),
                security: "tls".to_owned(),
                transport: "grpc".to_owned(),
                service_name: "svc".to_owned(),
                multi: true,
                ..ServerForm::default()
            },
            ServerForm {
                protocol: "ss".to_owned(),
                address: "a.example".to_owned(),
                port: "8388".to_owned(),
                method: "2022-blake3-aes-128-gcm".to_owned(),
                password: "QUFBQUFBQUFBQUFBQUFBQQ==".to_owned(),
                ..ServerForm::default()
            },
            ServerForm {
                protocol: "socks".to_owned(),
                address: "2001:db8::1".to_owned(),
                port: "1080".to_owned(),
                user: "uuuu".to_owned(),
                password: "pppp".to_owned(),
                ..ServerForm::default()
            },
            ServerForm {
                protocol: "hysteria2".to_owned(),
                address: "a.example".to_owned(),
                port: "443".to_owned(),
                password: "PPPP".to_owned(),
                sni: "a.example".to_owned(),
                obfs: "OOOO".to_owned(),
                hop: "20000-30000".to_owned(),
                ..ServerForm::default()
            },
            ServerForm {
                protocol: "wireguard".to_owned(),
                address: "203.0.113.9".to_owned(),
                port: "51820".to_owned(),
                private_key: WG_KEY.to_owned(),
                peer_key: WG_KEY.to_owned(),
                local_addresses: "10.0.0.2/32, fd00::2/128".to_owned(),
                reserved: "1,2,3".to_owned(),
                mtu: "1280".to_owned(),
                ..ServerForm::default()
            },
            ServerForm {
                protocol: "vless".to_owned(),
                address: "a.example".to_owned(),
                port: "443".to_owned(),
                id: UUID.to_owned(),
                security: "none".to_owned(),
                transport: "kcp".to_owned(),
                header: "wechat-video".to_owned(),
                seed: "SSSS".to_owned(),
                ..ServerForm::default()
            },
            ServerForm {
                protocol: "vless".to_owned(),
                address: "a.example".to_owned(),
                port: "443".to_owned(),
                id: UUID.to_owned(),
                security: "tls".to_owned(),
                transport: "xhttp".to_owned(),
                path: "/x".to_owned(),
                mode: "packet-up".to_owned(),
                extra: r#"{"xPaddingBytes":"100-1000"}"#.to_owned(),
                ..ServerForm::default()
            },
        ];
        for form in forms {
            let back = round_trip(&form);
            assert_eq!(back.protocol, form.protocol);
            assert_eq!(back.password, form.password, "{form:?}");
            assert_eq!(
                back.transport.is_empty(),
                form.transport.is_empty() && back.transport.is_empty()
            );
        }
    }

    #[test]
    fn a_form_short_of_a_field_says_which() {
        let mut form = ServerForm::new_server();
        assert_eq!(form.to_link().unwrap_err(), "укажите адрес сервера");
        form.address = "a.example".to_owned();
        assert_eq!(form.to_link().unwrap_err(), "укажите UUID пользователя");
        form.id = "not-a-uuid".to_owned();
        form.sni = "a.example".to_owned();
        form.public_key = PBK.to_owned();
        assert!(form.to_link().unwrap_err().contains("UUID"));
        form.id = UUID.to_owned();
        form.port = "70000".to_owned();
        assert_eq!(form.to_link().unwrap_err(), "порт — число от 1 до 65535");
        form.port = "443".to_owned();
        form.public_key = "short".to_owned();
        let why = form.to_link().unwrap_err();
        assert!(why.contains("ключ REALITY"), "{why}");
        assert!(!why.contains("short"), "the value is not quoted: {why}");
    }

    #[test]
    fn secrets_are_marked_and_kept_out_of_debug() {
        let form = ServerForm {
            address: "a.example".to_owned(),
            id: UUID.to_owned(),
            sni: "www.example.net".to_owned(),
            public_key: PBK.to_owned(),
            ..ServerForm::new_server()
        };
        let rows = form.rows();
        let uuid = rows.iter().find(|r| r.label == "UUID").unwrap();
        assert!(uuid.secret);
        assert!(!rows.iter().find(|r| r.label == "SNI").unwrap().secret);
        let shown = format!("{form:?} {rows:?}");
        assert!(!shown.contains("1111"), "{shown}");
    }
}
