//! OpenVPN's management interface, read from the side bliss is on.
//!
//! Lines of text, one notification each, in the format OpenVPN's
//! `doc/management-notes.txt` gives: `>STATE:`, `>LOG:`, `>HOLD:`,
//! `>PASSWORD:`, `>FATAL:`, answers that start `SUCCESS:` or `ERROR:`, and
//! the log history `log on all` prints before `END`. Only what bliss acts on
//! is told apart.

use std::net::{Ipv4Addr, SocketAddr};

/// One line from OpenVPN, as far as bliss cares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// Where the connection has got to.
    State(State),
    /// A line of OpenVPN's log: its flags (`I`, `F`, `N`, `W`, `D`) and text.
    Log { flags: String, text: String },
    /// Waiting to be told to go on.
    Hold,
    /// Asking for a password: what for, in its own words.
    Password(String),
    /// About to exit, and why.
    Fatal(String),
    /// Anything else: answers to commands, the end of the history, bytes.
    Other,
}

/// A `>STATE:` notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// OpenVPN's name for it: `CONNECTING`, `WAIT`, `AUTH`, `GET_CONFIG`,
    /// `ASSIGN_IP`, `ADD_ROUTES`, `CONNECTED`, `RECONNECTING`, `EXITING`,
    /// `RESOLVE`, `TCP_CONNECT`, `AUTH_PENDING`.
    pub name: String,
    /// `SUCCESS` or `ERROR` for `CONNECTED`, the reason for `RECONNECTING`
    /// and `EXITING`.
    pub detail: String,
    /// The adapter's address on the VPN, given with `ASSIGN_IP` and
    /// `CONNECTED`.
    pub local: Option<Ipv4Addr>,
}

pub fn parse(line: &str) -> Line {
    let line = line.trim_end_matches(['\r', '\n']);
    if let Some(rest) = line.strip_prefix(">STATE:") {
        let fields: Vec<&str> = rest.split(',').collect();
        let field = |n: usize| fields.get(n).copied().unwrap_or_default();
        return Line::State(State {
            name: field(1).to_owned(),
            detail: field(2).to_owned(),
            local: field(3).parse().ok(),
        });
    }
    if let Some(rest) = line.strip_prefix(">LOG:") {
        return log(rest).unwrap_or(Line::Other);
    }
    if line.starts_with(">HOLD:") {
        return Line::Hold;
    }
    if let Some(rest) = line.strip_prefix(">PASSWORD:") {
        return Line::Password(rest.to_owned());
    }
    if let Some(rest) = line.strip_prefix(">FATAL:") {
        return Line::Fatal(rest.to_owned());
    }
    // `log on all` prints the history without the prefix, until `END`.
    if line.starts_with(|c: char| c.is_ascii_digit()) {
        return log(line).unwrap_or(Line::Other);
    }
    Line::Other
}

/// `time,flags,text`, whose text may hold commas of its own.
fn log(rest: &str) -> Option<Line> {
    let mut fields = rest.splitn(3, ',');
    let time = fields.next()?;
    if time.is_empty() || !time.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let flags = fields.next()?;
    let text = fields.next()?;
    Some(Line::Log {
        flags: flags.to_owned(),
        text: text.to_owned(),
    })
}

/// What OpenVPN warns about bliss's own way of talking to it, at every
/// start, which the user can do nothing about: the port is on loopback, and
/// the caller is known by its process rather than by a password.
const ABOUT_BLISS: &[&str] = &["WARNING: Using --management on a TCP port WITHOUT passwords"];

/// Whether a log line is worth a line in bliss's own log: fatal, an error,
/// or a warning, unless it is about bliss itself.
pub fn worth_saying(flags: &str, text: &str) -> bool {
    flags.contains(['F', 'N', 'W']) && !ABOUT_BLISS.iter().any(|about| text.starts_with(about))
}

/// The DNS servers in the options a server pushed, if this log line is the
/// one OpenVPN writes when they arrive.
///
/// OpenVPN logs the whole `PUSH_REPLY` before `pull-filter` drops anything
/// from it, and it is the only place the servers are said once bliss has
/// told OpenVPN to leave the machine's DNS alone. Both spellings: the old
/// `dhcp-option DNS`, and 2.6's `dns server N address`. IPv4 only, like
/// everything through the VPN for now.
pub fn pushed_dns(text: &str) -> Option<Vec<SocketAddr>> {
    let options = text
        .strip_prefix("PUSH: Received control message: '")?
        .trim_end_matches('\'');
    let mut found = Vec::new();
    for option in options.split(',') {
        let words: Vec<&str> = option.split_whitespace().collect();
        match words.as_slice() {
            ["dhcp-option", "DNS", address, ..] => {
                if let Ok(address) = address.parse::<Ipv4Addr>() {
                    found.push(SocketAddr::from((address, 53)));
                }
            }
            ["dns", "server", _, "address", addresses @ ..] => {
                for address in addresses {
                    if let Some(address) = dns_address(address) {
                        found.push(address);
                    }
                }
            }
            _ => {}
        }
    }
    found.dedup();
    Some(found)
}

/// `10.8.0.1` or `10.8.0.1:5353`; an IPv6 one is passed over.
fn dns_address(text: &str) -> Option<SocketAddr> {
    if let Ok(address) = text.parse::<Ipv4Addr>() {
        return Some(SocketAddr::from((address, 53)));
    }
    match text.parse::<SocketAddr>() {
        Ok(address) if address.is_ipv4() => Some(address),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_read_with_their_address() {
        assert_eq!(
            parse(">STATE:1727600000,CONNECTED,SUCCESS,10.8.0.2,203.0.113.9,1194,,,\r\n"),
            Line::State(State {
                name: "CONNECTED".to_owned(),
                detail: "SUCCESS".to_owned(),
                local: Some(Ipv4Addr::new(10, 8, 0, 2)),
            })
        );
        assert_eq!(
            parse(">STATE:1727600000,RECONNECTING,ping-restart,,,,,"),
            Line::State(State {
                name: "RECONNECTING".to_owned(),
                detail: "ping-restart".to_owned(),
                local: None,
            })
        );
    }

    #[test]
    fn log_lines_keep_their_commas() {
        let text = "TLS Error: TLS key negotiation failed, check your network";
        assert_eq!(
            parse(&format!(">LOG:1727600000,N,{text}")),
            Line::Log {
                flags: "N".to_owned(),
                text: text.to_owned(),
            }
        );
        // The history `log on all` prints has no prefix.
        assert_eq!(
            parse("1727600000,I,OpenVPN 2.6.14 [git:v2.6.14]"),
            Line::Log {
                flags: "I".to_owned(),
                text: "OpenVPN 2.6.14 [git:v2.6.14]".to_owned(),
            }
        );
        assert_eq!(parse("END"), Line::Other);
        assert_eq!(parse("SUCCESS: hold release succeeded"), Line::Other);
        assert!(worth_saying("N", "x") && worth_saying("FW", "x") && !worth_saying("I", "x"));
        assert!(!worth_saying(
            "W",
            "WARNING: Using --management on a TCP port WITHOUT passwords is STRONGLY discouraged \
             and considered insecure"
        ));
    }

    #[test]
    fn prompts_and_the_end_are_told_apart() {
        assert_eq!(parse(">HOLD:Waiting for hold release:0"), Line::Hold);
        assert_eq!(
            parse(">PASSWORD:Need 'Private Key' password"),
            Line::Password("Need 'Private Key' password".to_owned())
        );
        assert_eq!(
            parse(">FATAL:Cannot open TUN/TAP dev"),
            Line::Fatal("Cannot open TUN/TAP dev".to_owned())
        );
    }

    #[test]
    fn dns_servers_come_out_of_the_push_line() {
        let old = "PUSH: Received control message: 'PUSH_REPLY,redirect-gateway def1,\
                   dhcp-option DNS 10.8.0.1,dhcp-option DNS 10.8.0.2,dhcp-option DOMAIN corp.example,\
                   route-gateway 10.8.0.1,topology subnet,ping 10,ifconfig 10.8.0.2 255.255.255.0'";
        assert_eq!(
            pushed_dns(old),
            Some(vec![
                SocketAddr::from(([10, 8, 0, 1], 53)),
                SocketAddr::from(([10, 8, 0, 2], 53)),
            ])
        );
        let new = "PUSH: Received control message: 'PUSH_REPLY,dns server 0 address 10.9.0.1 \
                   [2001:db8::53]:53,dns server 1 address 10.9.0.2:5353,dns search-domains corp.example'";
        assert_eq!(
            pushed_dns(new),
            Some(vec![
                SocketAddr::from(([10, 9, 0, 1], 53)),
                SocketAddr::from(([10, 9, 0, 2], 5353)),
            ])
        );
        assert_eq!(
            pushed_dns("PUSH: Received control message: 'PUSH_REPLY,ping 10'"),
            Some(Vec::new())
        );
        assert_eq!(pushed_dns("Initialization Sequence Completed"), None);
    }
}
