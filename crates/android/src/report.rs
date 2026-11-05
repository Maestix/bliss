//! The journal as it leaves the phone: «Поделиться журналом».
//!
//! A person sends it to whoever helps them — a friend, the issue tracker, a
//! chat — so nothing in it may lead to their server or their account. The
//! log lines name servers by their names already (AGENTS.md §5), but the
//! engine's own lines can carry a server's address, an error can quote a
//! link, and a REALITY server's borrowed name says which server it is. So:
//!
//! - every server's address and every name it shows the network (TLS and
//!   REALITY's `sni`), and every subscription's host, become «сервер» and
//!   «подписка»;
//! - anything with `://` in it is «ссылка»: a link, or a subscription's
//!   address, whose token is in the path;
//! - a UUID is «uuid», and any long run of letters and digits that could be
//!   a key or a token is «…».
//!
//! Sites the person opened stay: they are what the journal is read for, and
//! the person chose to send it.

/// What stands in for a server's address or name.
const SERVER: &str = "«сервер»";
const SUBSCRIPTION: &str = "«подписка»";

/// One line of the journal with everything secret taken out. `servers` are
/// the strings that name a server (addresses, SNIs), `subscriptions` their
/// hosts; both are matched without case.
pub fn redact(line: &str, servers: &[String], subscriptions: &[String]) -> String {
    let mut text = line.to_owned();
    for (names, word) in [(servers, SERVER), (subscriptions, SUBSCRIPTION)] {
        // Longest first, so a name inside another is not cut out of it.
        let mut names: Vec<&String> = names.iter().filter(|n| n.len() >= 3).collect();
        names.sort_by_key(|n| std::cmp::Reverse(n.len()));
        for name in names {
            text = replace_ignoring_case(&text, name, word);
        }
    }
    words(&text)
}

/// Every link, UUID and long token in `text`, replaced.
fn words(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    // A word is what a link, a key or an address can be made of; the rest
    // — spaces, quotes, brackets, Cyrillic — passes through as it is.
    let part = |c: char| c.is_ascii_alphanumeric() || "._:/@%?=&+-~#!$*,;".contains(c);
    for c in text.chars() {
        if part(c) {
            word.push(c);
        } else {
            out.push_str(&judge(&word));
            word.clear();
            out.push(c);
        }
    }
    out.push_str(&judge(&word));
    out
}

fn judge(word: &str) -> String {
    if word.contains("://") {
        return "«ссылка»".to_owned();
    }
    let mut out = String::with_capacity(word.len());
    let mut run = String::new();
    // Within a word, a run of letters, digits and the characters base64 and
    // UUIDs are made of; punctuation around it (a colon, a comma, the `=` of
    // `key=`, base64's padding) is kept.
    let token = |c: char| c.is_ascii_alphanumeric() || "+/_-".contains(c);
    for c in word.chars() {
        if token(c) {
            run.push(c);
        } else {
            out.push_str(&secret(&run));
            run.clear();
            out.push(c);
        }
    }
    out.push_str(&secret(&run));
    out
}

/// A run that looks like a credential, replaced; any other kept.
fn secret(run: &str) -> String {
    if is_uuid(run) {
        return "«uuid»".to_owned();
    }
    let letters = run.chars().filter(char::is_ascii_alphabetic).count();
    let digits = run.chars().filter(char::is_ascii_digit).count();
    let mixed_case =
        run.chars().any(|c| c.is_ascii_uppercase()) && run.chars().any(|c| c.is_ascii_lowercase());
    // Long and of several kinds of character: a key, a password, a token.
    // A long plain word or a long number is left alone.
    if run.len() >= 20 && ((letters > 0 && digits > 0) || mixed_case) {
        return "…".to_owned();
    }
    run.to_owned()
}

fn is_uuid(run: &str) -> bool {
    let parts: Vec<&str> = run.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn replace_ignoring_case(text: &str, name: &str, with: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while let Some(found) = lower[at..].find(&name) {
        let start = at + found;
        out.push_str(&text[at..start]);
        out.push_str(with);
        at = start + name.len();
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_and_its_names_do_not_leave_the_phone() {
        let servers = vec!["203.0.113.7".to_owned(), "www.example.net".to_owned()];
        let subs = vec!["sub.example.org".to_owned()];
        let line = "12:00:01 WARN xray: dial tcp 203.0.113.7:443: REALITY to WWW.example.net failed; sub.example.org said 403";
        let out = redact(line, &servers, &subs);
        assert!(!out.contains("203.0.113.7"), "{out}");
        assert!(!out.to_lowercase().contains("www.example.net"), "{out}");
        assert!(!out.contains("sub.example.org"), "{out}");
        assert!(
            out.contains("«сервер»:443") && out.contains("«подписка»"),
            "{out}"
        );
    }

    #[test]
    fn links_keys_and_uuids_are_taken_out() {
        let line = "import vless://11111111-2222-3333-4444-555555555555@203.0.113.9:443?pbk=AAAAbbbbCCCCdddd1111eeee#x \
                    and https://sub.example.org/api/SECRETTOKEN, uuid 11111111-2222-3333-4444-555555555555, \
                    key=PPPPqqqqRRRRssss2222tttt.";
        let out = redact(line, &[], &[]);
        for secret in ["vless", "SECRETTOKEN", "11111111", "PPPPqqqq", "AAAAbbbb"] {
            assert!(!out.contains(secret), "{secret} left in: {out}");
        }
        assert_eq!(out.matches("«ссылка»").count(), 2, "{out}");
        assert!(out.contains("uuid «uuid»,"), "{out}");
        assert!(out.contains("key=…."), "{out}");
    }

    #[test]
    fn what_the_journal_is_read_for_stays() {
        let line = "12:00:02 INFO tun #48 www.google.com:443: the rules send it elsewhere now; closed — 1500 ms";
        assert_eq!(redact(line, &["203.0.113.7".to_owned()], &[]), line);
        let line = "12:00:03 INFO tunnel: starting through «Амстердам-2»";
        assert_eq!(redact(line, &[], &[]), line);
    }
}
