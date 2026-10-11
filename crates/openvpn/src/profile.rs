//! The profile as OpenVPN reads it: the one the work network handed out,
//! rewritten so the VPN serves bliss's rules and nothing else.
//!
//! A work profile is written for a machine that belongs to the work network
//! while connected: the server may push a default route, which takes every
//! connection on the machine, and its DNS servers, which then hear every name
//! the machine looks up. bliss wants neither. It keeps the server's routes and
//! DNS settings off the machine (`pull-filter`, first thing in the file, where
//! the first matching filter wins), comments out the same things written in
//! the profile itself, and asks for one route of its own through the VPN, with
//! a metric no other connection would pick: the way out for the connections
//! bliss sends there by its rules, bound to the VPN's adapter
//! (`bliss_core::Through`).
//!
//! Everything else is left as written — ciphers, certificates, keepalives —
//! so a profile that works in OpenVPN's own client works here.

/// The largest profile taken. One is a few kilobytes of certificates.
pub const MAX_PROFILE: usize = 256 * 1024;

/// Why a profile cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("это не профиль клиента OpenVPN: в нём нет строки «client»")]
    NotClient,
    #[error("в профиле нет адреса сервера — строки «remote»")]
    NoRemote,
    #[error("профиль просит логин и пароль при подключении, а bliss пока так не умеет")]
    Login,
    #[error("профиль просит одноразовый код при подключении, а bliss пока так не умеет")]
    Code,
    #[error("профиль ссылается на отдельный файл («{0}»); нужен .ovpn, в который всё встроено")]
    External(String),
    #[error("в профиле не закрыт блок <{0}>")]
    Unclosed(String),
    #[error("файл больше {} КБ — это не профиль OpenVPN", MAX_PROFILE / 1024)]
    TooLarge,
}

/// What goes first: the server's routes and DNS settings are dropped, and
/// the first matching filter wins, so these come before any the profile has.
/// `route-method` is OpenVPN GUI's: pushed, it would take routes away from
/// the service, which is what adds them for a program without rights.
///
/// And a profile written for Windows carries options only Windows' OpenVPN
/// knows — `ip-win32` above all — which Linux's refuses outright as unknown;
/// `ignore-unknown-option` lets it pass them by with a warning instead. On
/// every system, so that the preamble is one length everywhere; where an
/// option is known the list is never asked. `route-method` is on it although
/// the user's profile has it and OpenVPN's source was read both ways — as
/// ignored off Windows and as Windows-only — since on the list it passes
/// either way.
const FILTERS: &str = "\
# Written by bliss from the profile added in its window. The server does not
# get the machine: its routes and DNS settings are dropped, and bliss asks its
# DNS servers itself, for the sites its rules send through this VPN.
pull-filter ignore \"redirect-gateway\"
pull-filter ignore \"redirect-private\"
pull-filter ignore \"route \"
pull-filter ignore \"route-ipv6 \"
pull-filter ignore \"route-method\"
pull-filter ignore \"dhcp-option\"
pull-filter ignore \"dns \"
pull-filter ignore \"block-outside-dns\"
pull-filter ignore \"register-dns\"
ignore-unknown-option ip-win32 route-method tap-sleep dhcp-renew dhcp-release dhcp-pre-release pause-exit show-net-up win-sys windows-driver allow-nonadmin

";

/// How many lines bliss writes before the profile's own, each of which it
/// then carries one for one: line `n` of the profile is line `PREAMBLE + n`
/// of what OpenVPN reads, and names it so in its errors.
pub const PREAMBLE: usize = lines_in(FILTERS);

const fn lines_in(text: &str) -> usize {
    let bytes = text.as_bytes();
    let (mut at, mut lines) = (0, 0);
    while at < bytes.len() {
        if bytes[at] == b'\n' {
            lines += 1;
        }
        at += 1;
    }
    lines
}

/// What goes last: the one route bliss wants through the VPN.
///
/// A default route, so a connection bound to the VPN's adapter can reach any
/// address the work network does; Windows looks for routes through the
/// interface a socket is bound to, and without one the dial fails. The metric
/// is far above an ordinary default route's, and a longer prefix wins before
/// metrics are compared at all, so every other connection keeps the way it
/// had — TUN adapters included, whose routes are halves of the address space.
///
/// Linux needs it only for a `dev tap` profile, and is better without it
/// otherwise. A socket bound to a tunnel with no route covering the
/// destination is sent down the tunnel anyway — the kernel takes the
/// destination to be on the link — while a second default route, however
/// high its metric, becomes the machine's own the moment the real one goes,
/// and then everything goes to the work network.
pub const ROUTE: &str = "route 0.0.0.0 0.0.0.0 vpn_gateway 9999";

/// Whether `ROUTE` goes into a profile whose device is a TAP (`tap`) one.
pub fn wants_route(tap: bool) -> bool {
    cfg!(windows) || tap
}

/// Directives that would give the machine to the server, run programs, or
/// fight bliss for control of OpenVPN. Commented out, not deleted, so the
/// file on disk still says what the profile asked for.
const DROPPED: &[&str] = &[
    // The machine's routes and names.
    "redirect-gateway",
    "redirect-private",
    "route",
    "route-ipv6",
    "route-noexec",
    "route-nopull",
    "dhcp-option",
    "dns",
    "block-outside-dns",
    "register-dns",
    // Programs run on events: Linux scripts in most profiles, which would
    // only fail here, and never something bliss should start.
    "script-security",
    "up",
    "down",
    "route-up",
    "route-pre-down",
    "ipchange",
    "tls-verify",
    "plugin",
    // bliss's own: how it talks to OpenVPN and where the log goes.
    "management",
    "management-client",
    "management-hold",
    "management-query-passwords",
    "management-signal",
    "management-forget-disconnect",
    "management-up-down",
    "management-external-key",
    "management-external-cert",
    "service",
    "log",
    "log-append",
    "syslog",
    "daemon",
];

/// Directives that name a file, unless the file is written into the profile.
const FILES: &[&str] = &[
    "ca",
    "cert",
    "key",
    "pkcs12",
    "tls-auth",
    "tls-crypt",
    "tls-crypt-v2",
    "secret",
    "extra-certs",
    "dh",
    "crl-verify",
    "http-proxy-user-pass",
];

/// Whether pasted text is an OpenVPN profile rather than server links: it has
/// the two lines every client profile has, `client` (or `tls-client`) and a
/// `remote`. A link never starts a line with either.
pub fn looks_like(text: &str) -> bool {
    let (mut client, mut remote) = (false, false);
    for line in text.lines() {
        let name = line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_start_matches("--");
        match name {
            "client" | "tls-client" => client = true,
            "remote" => remote = true,
            _ => {}
        }
    }
    client && remote
}

/// The profile OpenVPN is given, or why this one cannot be.
pub fn rewrite(profile: &str) -> Result<String, ProfileError> {
    if profile.len() > MAX_PROFILE {
        return Err(ProfileError::TooLarge);
    }
    let mut out = String::with_capacity(FILTERS.len() + profile.len() + 256);
    out.push_str(FILTERS);

    let mut block: Option<String> = None;
    let mut blocks: Vec<String> = Vec::new();
    let mut client = false;
    let (mut pull, mut tls_client) = (false, false);
    let mut remote = false;
    let mut login = false;
    let mut code = false;
    let mut tap = false;
    let mut external: Option<String> = None;

    for line in profile.lines() {
        let trimmed = line.trim();
        if let Some(open) = &block {
            out.push_str(line);
            out.push('\n');
            if trimmed.eq_ignore_ascii_case(&format!("</{open}>")) {
                block = None;
            }
            continue;
        }
        if let Some(tag) = trimmed
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
            .filter(|tag| !tag.is_empty())
        {
            let tag = tag.to_ascii_lowercase();
            // A <connection> holds directives of its own — `remote` among
            // them — so its lines are read like the profile's. Every other
            // block is a file written in, and its lines are only carried.
            if !tag.starts_with('/') && tag != "connection" {
                blocks.push(tag.clone());
                block = Some(tag);
            }
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            out.push_str(line);
            out.push('\n');
            continue;
        }

        let mut words = trimmed.split_whitespace();
        let name = words
            .next()
            .unwrap_or_default()
            .trim_start_matches("--")
            .to_ascii_lowercase();
        let argument = words.next();
        match name.as_str() {
            "client" => client = true,
            "pull" => pull = true,
            "tls-client" => tls_client = true,
            "remote" => remote = true,
            "auth-user-pass" => login = true,
            "static-challenge" => code = true,
            "askpass" => login = true,
            "dev" | "dev-type" => {
                tap |= argument.is_some_and(|a| a.to_ascii_lowercase().starts_with("tap"));
            }
            file if FILES.contains(&file)
                && argument.is_some_and(|a| !a.eq_ignore_ascii_case("[inline]")) =>
            {
                external.get_or_insert(file.to_owned());
            }
            _ => {}
        }
        if DROPPED.contains(&name.as_str()) {
            out.push_str("# bliss: ");
        }
        out.push_str(line);
        out.push('\n');
    }

    if let Some(open) = block {
        return Err(ProfileError::Unclosed(open));
    }
    if !(client || (pull && tls_client)) {
        return Err(ProfileError::NotClient);
    }
    if !remote {
        return Err(ProfileError::NoRemote);
    }
    // The login may be written into the profile, in which case nothing is
    // asked; a code never is.
    if code {
        return Err(ProfileError::Code);
    }
    if login && !blocks.iter().any(|b| b == "auth-user-pass") {
        return Err(ProfileError::Login);
    }
    if let Some(file) = external {
        return Err(ProfileError::External(file));
    }

    if wants_route(tap) {
        out.push_str("\n# bliss: one way out through the VPN, for what its rules send here.\n");
        out.push_str(ROUTE);
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A profile of the shape a work network hands out, with repeated-letter
    /// stand-ins for the key material.
    const WORK: &str = "\
client
tls-client
auth SHA256
data-ciphers AES-256-GCM:AES-128-GCM
remote-cert-tls server
tls-version-min 1.3
proto udp
remote vpn.example 1194
dev tun
resolv-retry 5
nobind
keepalive 5 30
persist-key
persist-tun
verb 3
reneg-sec 0
route-method exe
route-delay 2
<ca>
-----BEGIN CERTIFICATE-----
AAAAAAAAAAAAAAAA
-----END CERTIFICATE-----
</ca>
<cert>
-----BEGIN CERTIFICATE-----
CCCCCCCCCCCCCCCC
-----END CERTIFICATE-----
</cert>
<key>
-----BEGIN PRIVATE KEY-----
KKKKKKKKKKKKKKKK
-----END PRIVATE KEY-----
</key>
<tls-crypt>
-----BEGIN OpenVPN Static key V1-----
TTTTTTTTTTTTTTTT
-----END OpenVPN Static key V1-----
</tls-crypt>
";

    #[test]
    fn a_work_profile_keeps_everything_but_gains_the_filters_and_the_route() {
        let written = rewrite(WORK).expect("a usable profile");
        // Every line of the original is still there, key material included.
        for line in WORK.lines() {
            assert!(written.contains(line), "{line} was lost");
        }
        // The filters come before anything the profile says.
        let first = written.find("pull-filter ignore \"redirect-gateway\"");
        assert!(first.is_some_and(|at| at < written.find("client").expect("client")));
        assert_eq!(
            written.trim_end().ends_with(ROUTE),
            cfg!(windows),
            "a tun profile"
        );
        // A Windows-only option passes on Linux, and nothing new is refused
        // on Windows, whose OpenVPN knows every one of them.
        assert!(written.contains("\nignore-unknown-option ip-win32 "));
        // route-method stays: the service's own channel overrides it.
        assert!(written.contains("\nroute-method exe\n"));
    }

    /// Line `n` of a profile is line `PREAMBLE + n` of what OpenVPN reads,
    /// commented out or not, which is how a line named in its error is found
    /// again in the person's own file.
    #[test]
    fn every_line_keeps_its_place_after_the_preamble() {
        let profile = format!("{WORK}redirect-gateway def1\r\ncomp-lzo\n");
        let written = rewrite(&profile).expect("a usable profile");
        let written: Vec<&str> = written.lines().collect();
        for (n, line) in profile.lines().enumerate() {
            let theirs = written[PREAMBLE + n];
            assert!(
                theirs.ends_with(line),
                "line {}: {theirs:?} for {line:?}",
                n + 1
            );
        }
    }

    /// What would hand the machine to the server is commented out, however it
    /// is spelled; the lines inside a certificate are never read as directives.
    #[test]
    fn what_takes_the_machine_is_commented_out() {
        let profile = format!(
            "{WORK}redirect-gateway def1\nroute 10.0.0.0 255.0.0.0\n--dhcp-option DNS 10.0.0.53\n\
             block-outside-dns\nup /etc/openvpn/update-resolv-conf\nscript-security 2\n\
             management 127.0.0.1 7505\n<extra-certs>\nroute 1.2.3.4\n</extra-certs>\n"
        );
        let written = rewrite(&profile).expect("usable");
        for line in [
            "redirect-gateway def1",
            "route 10.0.0.0 255.0.0.0",
            "--dhcp-option DNS 10.0.0.53",
            "block-outside-dns",
            "up /etc/openvpn/update-resolv-conf",
            "script-security 2",
            "management 127.0.0.1 7505",
        ] {
            assert!(written.contains(&format!("# bliss: {line}\n")), "{line}");
        }
        assert!(
            written.contains("\nroute 1.2.3.4\n"),
            "a block's lines are its own"
        );
        assert!(
            written.contains("\nroute-delay 2\n"),
            "a prefix is not a match"
        );
    }

    #[test]
    fn what_cannot_work_is_refused_in_words() {
        let without = |needle: &str| WORK.replace(needle, "");
        assert_eq!(
            rewrite(&without("client\ntls-client\n")),
            Err(ProfileError::NotClient)
        );
        assert_eq!(
            rewrite(&without("remote vpn.example 1194\n")),
            Err(ProfileError::NoRemote)
        );
        assert_eq!(
            rewrite(&format!("{WORK}auth-user-pass\n")),
            Err(ProfileError::Login)
        );
        assert!(
            rewrite(&format!(
                "{WORK}auth-user-pass\n<auth-user-pass>\nuser\nPPPPPPPP\n</auth-user-pass>\n"
            ))
            .is_ok(),
            "a login written into the profile asks nothing"
        );
        assert_eq!(
            rewrite(&format!("{WORK}static-challenge \"Code\" 1\n")),
            Err(ProfileError::Code)
        );
        assert_eq!(
            rewrite(&format!("{WORK}ca ca.crt\n")),
            Err(ProfileError::External("ca".to_owned()))
        );
        assert!(rewrite(&format!("{WORK}ca [inline]\n")).is_ok());
        assert_eq!(
            rewrite(&format!("{WORK}<key>\nKKKK\n")),
            Err(ProfileError::Unclosed("key".to_owned()))
        );
        assert_eq!(
            rewrite(&"#".repeat(MAX_PROFILE + 1)),
            Err(ProfileError::TooLarge)
        );
    }

    /// A profile pasted with Ctrl+V is told from links and lists.
    #[test]
    fn a_pasted_profile_is_recognised() {
        assert!(looks_like(WORK));
        assert!(!looks_like(
            "vless://11111111-2222-3333-4444-555555555555@a.example:443?encryption=none"
        ));
        assert!(!looks_like("https://sub.example/list"));
        assert!(!looks_like("client only, no server"));
    }

    /// A `remote` inside a <connection> is a remote.
    #[test]
    fn connection_blocks_are_read() {
        let profile = WORK.replace(
            "remote vpn.example 1194\n",
            "<connection>\nremote vpn.example 1194 udp\n</connection>\n",
        );
        let written = rewrite(&profile).expect("usable");
        assert!(written.contains("<connection>\nremote vpn.example 1194 udp\n</connection>\n"));
    }

    /// Windows line endings survive: the certificate blocks are compared by
    /// their closing tag, not by the exact line.
    #[test]
    fn crlf_profiles_are_read_the_same() {
        let crlf = WORK.replace('\n', "\r\n");
        let written = rewrite(&crlf).expect("usable");
        assert!(written.contains("KKKKKKKKKKKKKKKK"));
        assert_eq!(written.trim_end().ends_with(ROUTE), cfg!(windows));
    }

    /// Every option a profile written for Windows may carry and Linux's
    /// OpenVPN may not know is either commented out or passed by: the user's
    /// own profile has `route-method exe`, and an older one `dhcp-release`.
    #[test]
    fn windows_only_options_never_stop_linux() {
        let written = rewrite(WORK).expect("a usable profile");
        let ignored: Vec<&str> = written
            .lines()
            .find_map(|line| line.strip_prefix("ignore-unknown-option "))
            .expect("the line is there")
            .split_whitespace()
            .collect();
        for option in [
            "route-method",
            "ip-win32",
            "dhcp-release",
            "dhcp-renew",
            "dhcp-pre-release",
            "tap-sleep",
            "pause-exit",
            "show-net-up",
            "win-sys",
            "windows-driver",
            "allow-nonadmin",
        ] {
            assert!(
                ignored.contains(&option),
                "{option} would stop Linux's OpenVPN"
            );
        }
        for dropped in ["block-outside-dns", "register-dns"] {
            assert!(DROPPED.contains(&dropped), "{dropped} is commented out");
        }
    }

    /// A TAP device answers no packet for an address it has no route to, so
    /// a TAP profile gets the route everywhere; a TUN one on Windows alone.
    #[test]
    fn the_route_goes_where_it_is_needed() {
        let tap = rewrite(&WORK.replace("dev tun\n", "dev tap0\n")).expect("usable");
        assert!(tap.trim_end().ends_with(ROUTE), "a tap profile, anywhere");
        let tun = rewrite(WORK).expect("usable");
        assert_eq!(tun.trim_end().ends_with(ROUTE), cfg!(windows));
    }
}
