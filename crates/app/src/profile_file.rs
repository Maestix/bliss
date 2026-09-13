//! The profile on disk: reading it, creating it, adding to it.
//!
//! Every write bliss makes to the user's file goes through here, and each one
//! keeps three promises:
//!
//! - **Created on first use, not on first run.** The file appears when the
//!   window has something to put in it — a server, the starting rules. Opening
//!   bliss leaves nothing behind, and a profile that exists was asked for.
//! - **Appended, never re-serialised.** A serde round-trip would delete every
//!   comment in the file, including the ones explaining what the user is
//!   looking at. New text goes at the end; the rest stays theirs.
//! - **Checked before it is written.** The whole new text is parsed first and
//!   replaces the file in one rename, so a refusal or a failure leaves the old
//!   file exactly as it was — never half-written, never unparseable.

use std::fmt;
use std::path::{Path, PathBuf};

use bliss_proto::{LinkError, Profile, ServerConfig, ServerEntry};

/// What a new profile starts as: every setting at its default, and what each
/// one does. `links.rs` holds the defaults it documents to the code's.
pub const TEMPLATE: &str = include_str!("../../../bliss.example.toml");

/// Why the profile on disk could not be used.
#[derive(Debug)]
pub enum ProfileError {
    Unreadable(PathBuf, std::io::Error),
    Invalid {
        path: PathBuf,
        /// From 1, where the parser gave up.
        line: Option<usize>,
        error: Box<toml::de::Error>,
    },
}

impl ProfileError {
    /// One line, safe to put on screen.
    ///
    /// A TOML error's full text quotes the offending line of the file, and in a
    /// profile that line can be a server link with its UUID and keys in it —
    /// the one thing that must not end up in a screenshot.
    pub fn brief(&self) -> String {
        match self {
            Self::Unreadable(path, e) => format!("{}: {e}", path.display()),
            Self::Invalid { path, line, error } => {
                let message = error.message();
                match line {
                    Some(line) => format!("{}, строка {line}: {message}", path.display()),
                    None => format!("{}: {message}", path.display()),
                }
            }
        }
    }
}

/// The full text, for the terminal — where the quoted line is what helps.
impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable(path, e) => {
                write!(f, "cannot read the profile at {}: {e}", path.display())
            }
            Self::Invalid { path, error, .. } => {
                write!(
                    f,
                    "{} is not a valid bliss profile: {error}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ProfileError {}

/// Read the profile. `Ok(None)` when there is no file yet, which is the normal
/// state before the first server is added. Never creates anything.
pub fn read(path: &Path) -> Result<Option<Profile>, ProfileError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(path, &text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ProfileError::Unreadable(path.to_owned(), e)),
    }
}

/// Parse profile text read from `path`.
pub fn parse(path: &Path, text: &str) -> Result<Profile, ProfileError> {
    toml::from_str(text).map_err(|error| ProfileError::Invalid {
        path: path.to_owned(),
        line: error
            .span()
            .map(|span| text[..span.start.min(text.len())].matches('\n').count() + 1),
        error: Box::new(error),
    })
}

/// Add a server from a pasted link, creating the profile if there is none.
///
/// Refuses, writing nothing, when the link does not parse, when the profile
/// already holds it, or when the profile on disk does not parse — appending to
/// a broken file would only bury the problem under a server.
pub fn add_server(path: &Path, link: &str) -> Result<(ServerEntry, ServerConfig), String> {
    let link = link.trim();
    if link.is_empty() {
        return Err("вставьте ссылку vless://".to_owned());
    }
    let config: ServerConfig = link.parse().map_err(|e| link_problem(link, &e))?;

    let existing = current(path)?;
    let before = parse(path, &existing).map_err(broken)?;
    if before.servers.iter().any(|s| s.link.trim() == link) {
        return Err(format!("этот сервер уже есть в профиле: «{}»", config.tag));
    }

    let text = appended(&existing, &server_block(link)?);
    let after =
        parse(path, &text).map_err(|e| format!("не получилось дописать сервер: {}", e.brief()))?;
    // Appending one [[server]] adds exactly one server to a profile written the
    // usual way. A profile that spells its servers as an inline array would
    // reject the append above; this catches anything stranger than that.
    if after.servers.len() != before.servers.len() + 1 {
        return Err("не получилось дописать сервер: профиль устроен необычно — \
                    добавьте ссылку в файл вручную"
            .to_owned());
    }
    replace(path, &text)?;

    let entry = ServerEntry {
        link: link.to_owned(),
        name: None,
        group: None,
    };
    Ok((entry, config))
}

/// Append the starting set of rules, creating the profile if there is none.
///
/// Refuses if the profile already says anything about routing. Whatever is in
/// there is a decision someone made, and a second `[routing]` would not parse.
pub fn add_starting_rules(path: &Path) -> Result<(), String> {
    let existing = current(path)?;
    parse(path, &existing).map_err(broken)?;
    if existing.lines().any(is_routing_header) {
        return Err("в профиле уже есть раздел [routing]".to_owned());
    }
    let text = appended(&existing, STARTING_RULES);
    parse(path, &text).map_err(|e| format!("не получилось дописать правила: {}", e.brief()))?;
    replace(path, &text)
}

/// Does the profile have a `[routing]` section?
///
/// Read from the text rather than from the parsed profile, because serde fills
/// a missing section in with defaults and the result is indistinguishable from
/// an empty one someone wrote on purpose. The difference matters: it decides
/// whether the screen offers to write a starting set or leaves the user's
/// deliberate choices alone.
pub fn has_routing_section(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| text.lines().any(is_routing_header))
        .unwrap_or(false)
}

pub fn is_routing_header(line: &str) -> bool {
    let line = line.trim();
    line == "[routing]" || line.starts_with("[[routing.")
}

fn broken(e: ProfileError) -> String {
    format!("профиль не читается — сначала исправьте его. {}", e.brief())
}

/// The profile's text as it stands, or the template if there is no profile yet.
fn current(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TEMPLATE.to_owned()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn appended(existing: &str, addition: &str) -> String {
    let mut text = existing.to_owned();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(addition);
    text
}

/// `[[server]]` with the link in it, quoted by the TOML writer rather than by
/// hand: the name part of a link can hold quotes and backslashes.
fn server_block(link: &str) -> Result<String, String> {
    let mut server = toml::Table::new();
    server.insert("link".to_owned(), toml::Value::String(link.to_owned()));
    let mut root = toml::Table::new();
    root.insert(
        "server".to_owned(),
        toml::Value::Array(vec![toml::Value::Table(server)]),
    );
    let block =
        toml::to_string(&root).map_err(|e| format!("не получилось записать ссылку: {e}"))?;
    Ok(format!("\n{block}"))
}

/// Put `text` in place of the file in one step.
///
/// Written beside it and renamed over it, so a failure midway leaves the old
/// profile rather than part of a new one. On Windows `rename` replaces an
/// existing file, so nothing is deleted first.
fn replace(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let staged = path.with_extension("toml.new");
    std::fs::write(&staged, text).map_err(|e| format!("{}: {e}", staged.display()))?;
    std::fs::rename(&staged, path).map_err(|e| {
        let _ = std::fs::remove_file(&staged);
        format!("{}: {e}", path.display())
    })
}

/// Why a pasted link was refused, in words for the window.
///
/// By variant rather than from the error's text, which is English and meant
/// for the log. And never with the link in it: a value that failed to parse
/// can be most of a key.
fn link_problem(link: &str, e: &LinkError) -> String {
    match e {
        LinkError::Scheme(scheme) if scheme == "https" || scheme == "http" => {
            "это адрес страницы, а не ссылка на сервер. Если это подписка — bliss их \
             пока не умеет; добавьте сервер ссылкой vless://"
                .to_owned()
        }
        LinkError::Scheme(scheme) => {
            format!("ссылки {scheme}:// bliss пока не понимает — нужна ссылка vless://")
        }
        LinkError::Url(_) if !link.contains("://") => {
            "это не похоже на ссылку — она начинается с vless://".to_owned()
        }
        LinkError::Url(_) => "ссылка повреждена — скопируйте её заново целиком".to_owned(),
        LinkError::MissingUuid | LinkError::Uuid(_) => {
            "в ссылке нет правильного UUID пользователя — скопируйте её заново целиком".to_owned()
        }
        LinkError::MissingHost => "в ссылке нет адреса сервера".to_owned(),
        LinkError::MissingPort => "в ссылке нет порта сервера".to_owned(),
        LinkError::RealityMissing(param) => {
            format!("для REALITY в ссылке не хватает параметра {param}")
        }
        LinkError::BadValue {
            param, expected, ..
        } => {
            format!("в ссылке неверный параметр {param}: ожидается {expected}")
        }
        LinkError::UnsupportedTransport(transport) => {
            format!("транспорт {transport} bliss пока не поддерживает — только TCP")
        }
        LinkError::Encryption(_) => "в ссылке не разбирается параметр encryption".to_owned(),
    }
}

/// What a first set of rules should be, for someone who has not written any.
///
/// Three rules and a comment each, because a set nobody can read is one nobody
/// will edit. No country-wide site list: those are matters of taste, and this
/// is meant to be uncontroversial — advertising blocked, the local network not
/// sent through a tunnel to come back again.
const STARTING_RULES: &str = r#"
# --- Маршрутизация ---------------------------------------------------------
#
# Правила читаются сверху вниз, срабатывает первое подошедшее.
# Проверить, ничего не подключая: bliss route youtube.com
#
# Это стартовый набор, добавленный из окна. Правьте как угодно — и нажмите
# «Перечитать» в разделе «Маршрутизация», чтобы изменения вступили в силу.

[routing]
# Куда идёт всё, что не подошло ни под одно правило.
fallback = "proxy"

[[routing.rule]]
name = "Реклама и трекеры"
action = "block"
# Готовая категория из geosite.dat. Работает на всю систему, а не только
# в браузере.
geosite = ["category-ads-all"]

[[routing.rule]]
name = "Локальная сеть"
action = "direct"
# Домашний роутер, принтер, соседний компьютер: тащить их через туннель
# на другой континент и обратно незачем.
ip = ["127.0.0.0/8", "10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]

# Дальше — примеры. Раскомментируйте то, что нужно.
#
# [[routing.rule]]
# name = "Торренты напрямую"
# action = "direct"
# process = ["qbittorrent.exe", "utorrent.exe"]
#
# [[routing.rule]]
# name = "Свои сайты напрямую"
# action = "direct"
# # Без префикса домен значит «он и всё, что под ним». Ещё есть
# # full: — ровно это имя, и keyword: — текст где угодно в имени.
# domain = ["gosuslugi.ru", "keyword:sberbank"]
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use bliss_proto::Action;

    // Fake by construction: reserved names and a repeated-letter key.
    const FIRST: &str =
        "vless://11111111-2222-3333-4444-555555555555@a.example:443?encryption=none#first";
    const SECOND: &str =
        "vless://11111111-2222-3333-4444-555555555555@b.example:8443?encryption=none#second";

    /// A profile path in a directory of its own, emptied first, so tests can
    /// run side by side and a failed run leaves nothing for the next to trip on.
    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("bliss-profile-tests").join(test);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("bliss.toml")
    }

    fn text(path: &Path) -> String {
        std::fs::read_to_string(path).expect("read the profile back")
    }

    #[test]
    fn looking_for_a_profile_does_not_create_one() {
        let path = scratch("read-missing");
        assert!(read(&path).expect("no profile is not an error").is_none());
        assert!(!path.exists());
    }

    #[test]
    fn the_first_server_creates_the_profile_from_the_template() {
        let path = scratch("first-server");
        let (entry, config) = add_server(&path, &format!("  {FIRST}\r\n")).expect("added");
        assert_eq!(
            entry.link, FIRST,
            "surrounding whitespace is not part of the link"
        );
        assert_eq!(config.tag, "first");

        let written = text(&path);
        assert!(
            written.starts_with(TEMPLATE),
            "a new profile is the explained template"
        );
        let profile = read(&path).expect("parses").expect("exists");
        assert_eq!(profile.servers.len(), 1);
        assert_eq!(profile.servers[0].link, FIRST);
        assert!(
            !has_routing_section(&path),
            "the routing screen can still offer the starting rules"
        );
        assert!(
            !path.with_extension("toml.new").exists(),
            "nothing left beside it"
        );
    }

    #[test]
    fn a_server_is_appended_and_every_comment_survives() {
        let path = scratch("append-server");
        let before = format!(
            "# my profile\n[inbound]\nsocks = 10808 # the usual\n\n[[server]]\n# the one I use\nlink = \"{SECOND}\"\n"
        );
        std::fs::write(&path, &before).expect("write");

        add_server(&path, FIRST).expect("added");
        let after = text(&path);
        assert!(after.starts_with(&before), "the original is untouched");
        let profile = read(&path).expect("parses").expect("exists");
        let links: Vec<&str> = profile.servers.iter().map(|s| s.link.as_str()).collect();
        assert_eq!(links, [SECOND, FIRST], "in the order they were added");
    }

    /// Quoted by the TOML writer: a name with quotes, a backslash and Cyrillic
    /// must come back from the file exactly as it went in.
    #[test]
    fn the_link_goes_in_exactly_as_pasted() {
        let path = scratch("exact-link");
        let link = format!(
            "{}#он сказал {q}да{q} {b} и ушёл",
            FIRST.trim_end_matches("#first"),
            q = '"',
            b = char::from(92)
        );
        add_server(&path, &link).expect("added");
        let profile = read(&path).expect("parses").expect("exists");
        assert_eq!(profile.servers[0].link, link);
    }

    #[test]
    fn a_profile_that_does_not_parse_is_not_written_to() {
        let path = scratch("broken-profile");
        // An unterminated string in the middle of a link: the parser's full
        // message would quote this line, key and all.
        let before = "[[server]]\nlink = \"vless://11111111-2222-3333-4444-555555555555@a.example:443\
                      ?security=reality&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n";
        std::fs::write(&path, before).expect("write");

        let refused = add_server(&path, FIRST).expect_err("a broken profile is not appended to");
        assert!(refused.contains("исправьте"), "says what to do: {refused}");
        assert!(
            !refused.contains("AAAAAAAA") && !refused.contains("11111111"),
            "{refused}"
        );
        assert!(add_starting_rules(&path).is_err());
        assert_eq!(text(&path), before, "not a byte changed");
        assert!(!path.with_extension("toml.new").exists());
    }

    #[test]
    fn the_same_server_twice_is_refused() {
        let path = scratch("duplicate");
        add_server(&path, FIRST).expect("first time");
        let written = text(&path);
        let refused = add_server(&path, FIRST).expect_err("second time");
        assert!(refused.contains("уже есть"), "{refused}");
        assert_eq!(text(&path), written);
    }

    #[test]
    fn a_refused_link_is_explained_without_being_repeated() {
        let path = scratch("bad-links");
        for (link, expect) in [
            (
                "https://sub.example/api/subscribe?token=SECRETTOKEN",
                "подписк",
            ),
            ("trojan://SECRETTOKEN@a.example:443", "trojan"),
            ("SECRETTOKEN", "не похоже на ссылку"),
            (
                "vless://11111111-2222-3333-4444-555555555555@a.example:443?security=reality&sni=b.example&sid=SECRETTOKEN",
                "REALITY",
            ),
        ] {
            let refused = add_server(&path, link).expect_err(link);
            assert!(refused.contains(expect), "{link}: {refused}");
            assert!(!refused.contains("SECRETTOKEN"), "{link}: {refused}");
        }
        assert!(!path.exists(), "a refused link creates no profile");
    }

    /// The one button that writes to the user's file besides adding a server,
    /// so the result had better parse. If it did not, a click would leave a
    /// profile the program cannot read.
    #[test]
    fn the_starting_rules_append_to_a_profile_and_still_parse() {
        let path = scratch("starting-rules");
        // A profile with comments and a server in it, which is what a real one
        // looks like by the time anyone presses the button.
        let before = format!(
            "# my profile\n[inbound]\nsocks = 10808\nhttp = 10809\n\n[[server]]\n# the one I use\nlink = \"{FIRST}\"\n"
        );
        std::fs::write(&path, &before).expect("write");

        add_starting_rules(&path).expect("appending the starting rules");
        let after = text(&path);

        // Nothing of theirs was lost — this is why it appends rather than
        // serialising the profile back over itself.
        assert!(after.starts_with(&before), "the original must be untouched");
        assert!(after.contains("# the one I use"), "comments survive");

        let profile = read(&path)
            .expect("the result must be a valid profile")
            .expect("exists");
        assert_eq!(profile.routing.rules.len(), 2);
        assert_eq!(profile.routing.fallback, Action::Proxy);
        assert_eq!(profile.routing.rules[0].action, Action::Block);
        assert_eq!(profile.routing.rules[0].geosite, ["category-ads-all"]);
        assert_eq!(profile.routing.rules[1].action, Action::Direct);
        assert_eq!(profile.servers.len(), 1, "the server is still there");

        // And it refuses to do it twice, because two [routing] sections are not
        // a profile at all.
        assert!(add_starting_rules(&path).is_err());
        assert_eq!(
            text(&path),
            after,
            "a refused append must not have written anything"
        );
    }

    /// The routing screen offers the starting rules before there is a profile,
    /// and a server added afterwards has to land after them and still parse.
    #[test]
    fn the_starting_rules_can_create_the_profile_too() {
        let path = scratch("rules-first");
        add_starting_rules(&path).expect("creates the profile");
        assert!(text(&path).starts_with(TEMPLATE));
        assert!(has_routing_section(&path));

        add_server(&path, FIRST).expect("a server after the rules");
        let profile = read(&path).expect("parses").expect("exists");
        assert_eq!(profile.routing.rules.len(), 2);
        assert_eq!(profile.servers.len(), 1);
    }

    /// A missing section and an empty one look identical after serde fills the
    /// defaults in, and they mean different things: one is "never decided", the
    /// other is "decided to have none".
    #[test]
    fn a_written_routing_section_is_told_apart_from_an_absent_one() {
        let path = scratch("detect-routing");
        for (text, configured) in [
            ("[inbound]\nsocks = 10808\n", false),
            ("[routing]\nfallback = \"proxy\"\n", true),
            ("[inbound]\nsocks = 1\n\n  [routing]  \n", true),
            ("[[routing.rule]]\naction = \"block\"\n", true),
            // Mentioned in prose is not the same as written.
            ("# see [routing] below one day\n[inbound]\n", false),
        ] {
            std::fs::write(&path, text).expect("write");
            assert_eq!(has_routing_section(&path), configured, "for {text:?}");
        }
        assert!(!has_routing_section(&path.with_file_name("absent.toml")));
    }

    /// The template is what every new profile starts as. It has to be a valid
    /// profile with nothing in it, and it must leave routing unwritten, or the
    /// starting rules could never be added to a profile the window created.
    #[test]
    fn the_template_is_an_empty_profile_with_routing_left_open() {
        let profile: Profile = toml::from_str(TEMPLATE).expect("the template parses");
        assert!(profile.servers.is_empty());
        assert!(profile.routing.rules.is_empty());
        assert!(!TEMPLATE.lines().any(is_routing_header));
    }
}
