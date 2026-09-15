//! The profile: bliss's own settings file, and the changes the window makes to it.
//!
//! The file belongs to bliss, the way v2rayN's configuration belongs to v2rayN.
//! Everything is changed in the window, and bliss writes the whole file after
//! every change; nobody is expected to open it. That is what lets the window
//! change anything at all, instead of tiptoeing around the comments of a file
//! someone wrote by hand.
//!
//! - **Written on the first change, never on launch.** Opening bliss leaves
//!   nothing behind.
//! - **Written whole, in one rename.** The new text goes beside the file and
//!   replaces it in one step, so a failure leaves the old profile rather than
//!   part of a new one.
//! - **A file bliss did not write is kept.** Before the first write in a run, a
//!   profile without bliss's header — made by hand, or from before the window
//!   owned it — or one that does not parse is copied beside itself as
//!   `bliss.toml.bak`. An existing backup is never overwritten.

use std::fmt;
use std::path::{Path, PathBuf};

use bliss_proto::{Action, LinkError, Profile, Rule, ServerConfig, ServerEntry};

/// The first lines of every profile bliss writes, which is also how it tells
/// its own file from anyone else's.
pub const HEADER: &str = "# Written by bliss. Change settings in its window: this file is rewritten\n\
                          # whole after every change, and nothing added here by hand is kept.\n\n";

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
/// state before anything has been changed in the window. Never creates it.
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

/// Where the profile is saved, and whether this run has already made sure the
/// file it replaces is bliss's own.
pub struct Store {
    path: PathBuf,
    checked: bool,
}

impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            checked: false,
        }
    }

    /// Write the whole profile, returning the backup made of the file it
    /// replaced, if that file was not bliss's own.
    pub fn save(&mut self, profile: &Profile) -> Result<Option<PathBuf>, String> {
        let body = toml::to_string_pretty(profile)
            .map_err(|e| format!("не получилось записать профиль: {e}"))?;
        let text = format!("{HEADER}{body}");
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }

        let backup = if self.checked {
            None
        } else {
            let backup = self.keep_if_foreign()?;
            // Settled for the rest of the run even if the write below fails:
            // the file, if it needed keeping, has been kept.
            self.checked = true;
            backup
        };

        // Written beside it and renamed over it, so a failure midway leaves the
        // old profile. On Windows `rename` replaces an existing file, so
        // nothing is deleted first.
        let staged = self.path.with_extension("toml.new");
        std::fs::write(&staged, text).map_err(|e| format!("{}: {e}", staged.display()))?;
        std::fs::rename(&staged, &self.path).map_err(|e| {
            let _ = std::fs::remove_file(&staged);
            format!("{}: {e}", self.path.display())
        })?;
        Ok(backup)
    }

    /// Copy the file about to be replaced beside itself, unless bliss wrote it.
    fn keep_if_foreign(&self) -> Result<Option<PathBuf>, String> {
        let existing = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        let ours = std::str::from_utf8(&existing)
            .is_ok_and(|text| text.starts_with(HEADER) && parse(&self.path, text).is_ok());
        if ours {
            return Ok(None);
        }
        let backup = free_backup_path(&self.path);
        std::fs::write(&backup, &existing).map_err(|e| format!("{}: {e}", backup.display()))?;
        Ok(Some(backup))
    }
}

/// `bliss.toml.bak`, or the first of `bliss.toml.1.bak`, `.2.bak`, … not taken.
fn free_backup_path(path: &Path) -> PathBuf {
    std::iter::once(path.with_extension("toml.bak"))
        .chain((1..).map(|n| path.with_extension(format!("toml.{n}.bak"))))
        .find(|candidate| !candidate.exists())
        .expect("some number is free")
}

/// Add a server from a pasted link. Leaves the profile as it was when the link
/// does not parse or the profile already holds it.
pub fn add_server(profile: &mut Profile, link: &str) -> Result<ServerConfig, String> {
    let link = link.trim();
    if link.is_empty() {
        return Err("вставьте ссылку vless://".to_owned());
    }
    let config: ServerConfig = link.parse().map_err(|e| link_problem(link, &e))?;
    if profile.servers.iter().any(|s| s.link.trim() == link) {
        return Err(format!("этот сервер уже есть: «{}»", config.tag));
    }
    profile.servers.push(ServerEntry {
        link: link.to_owned(),
        name: None,
        group: None,
    });
    Ok(config)
}

/// Put the starting set of rules in a profile that has none.
///
/// Refuses rather than replacing rules that are already there: they are
/// choices someone made.
pub fn add_starting_rules(profile: &mut Profile) -> Result<(), String> {
    if !profile.routing.rules.is_empty() {
        return Err("правила уже есть — стартовый набор их не заменяет".to_owned());
    }
    profile.routing.rules = starting_rules();
    Ok(())
}

/// What a first set of rules should be, for someone who has not made any.
///
/// Uncontroversial on purpose: advertising blocked, and the local network not
/// sent through a tunnel to come back again. Country-wide site lists are
/// matters of taste, and do not belong in a default.
fn starting_rules() -> Vec<Rule> {
    let list = |items: &[&str]| items.iter().map(|item| (*item).to_owned()).collect();
    vec![
        Rule {
            name: Some("Реклама и трекеры".to_owned()),
            action: Action::Block,
            domain: Vec::new(),
            ip: Vec::new(),
            port: Vec::new(),
            process: Vec::new(),
            geosite: list(&["category-ads-all"]),
            geoip: Vec::new(),
        },
        Rule {
            name: Some("Локальная сеть".to_owned()),
            action: Action::Direct,
            domain: Vec::new(),
            ip: list(&[
                "127.0.0.0/8",
                "10.0.0.0/8",
                "172.16.0.0/12",
                "192.168.0.0/16",
            ]),
            port: Vec::new(),
            process: Vec::new(),
            geosite: Vec::new(),
            geoip: Vec::new(),
        },
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    // Fake by construction: reserved names and repeated-letter values.
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
        std::fs::read_to_string(path).expect("read the file back")
    }

    fn links(profile: &Profile) -> Vec<&str> {
        profile.servers.iter().map(|s| s.link.as_str()).collect()
    }

    #[test]
    fn looking_for_a_profile_does_not_create_one() {
        let path = scratch("read-missing");
        assert!(read(&path).expect("no profile is not an error").is_none());
        assert!(!path.exists());
    }

    /// The profile most people save first: one server, no rules. The window
    /// run found `rule = []` in it — the round trip below has rules, so it
    /// could not.
    #[test]
    fn a_profile_without_rules_saves_without_empty_lists() {
        let path = scratch("no-rules");
        let mut profile = Profile::default();
        add_server(&mut profile, FIRST).expect("added");
        Store::new(path.clone()).save(&profile).expect("saved");
        let written = text(&path);
        assert!(
            !written.contains("= []"),
            "empty lists are left out:\n{written}"
        );
        assert!(
            read(&path)
                .expect("parses")
                .expect("exists")
                .routing
                .rules
                .is_empty()
        );
    }

    #[test]
    fn a_saved_profile_reads_back_as_it_was() {
        let path = scratch("round-trip");
        let mut profile = Profile::default();
        let server = add_server(&mut profile, &format!("  {FIRST}\r\n")).expect("added");
        assert_eq!(server.tag, "first");
        add_starting_rules(&mut profile).expect("rules");
        profile.inbound.system_proxy = bliss_proto::ProxyMode::Set;

        let mut store = Store::new(path.clone());
        assert_eq!(
            store.save(&profile),
            Ok(None),
            "no file before, nothing to keep"
        );

        let written = text(&path);
        assert!(written.starts_with(HEADER));
        assert!(
            !written.contains("= []"),
            "empty lists are left out:\n{written}"
        );
        let back = read(&path).expect("parses").expect("exists");
        assert_eq!(
            links(&back),
            [FIRST],
            "surrounding whitespace is not part of the link"
        );
        assert_eq!(back.inbound.system_proxy, bliss_proto::ProxyMode::Set);
        assert_eq!(back.routing.rules.len(), 2);
        assert_eq!(back.routing.rules[0].action, Action::Block);
        assert_eq!(back.routing.rules[0].geosite, ["category-ads-all"]);
        assert_eq!(back.routing.rules[1].action, Action::Direct);
        assert!(
            !path.with_extension("toml.new").exists(),
            "nothing left beside it"
        );
        assert!(!path.with_extension("toml.bak").exists());
    }

    /// The switch from a file people wrote to a file bliss writes: the old one,
    /// comments and all, has to survive somewhere.
    #[test]
    fn a_hand_made_profile_is_kept_before_the_first_write() {
        let path = scratch("hand-made");
        let before = format!(
            "# my profile\n[inbound]\nsocks = 10808 # the usual\n\n[[server]]\n# mine\nlink = \"{SECOND}\"\n"
        );
        std::fs::write(&path, &before).expect("write");

        let mut profile = read(&path).expect("parses").expect("exists");
        add_server(&mut profile, FIRST).expect("added");
        let mut store = Store::new(path.clone());
        let backup = store
            .save(&profile)
            .expect("saved")
            .expect("a backup was made");
        assert_eq!(backup, path.with_extension("toml.bak"));
        assert_eq!(text(&backup), before, "kept byte for byte");
        assert_eq!(links(&read(&path).unwrap().unwrap()), [SECOND, FIRST]);

        // Once per run, and never again for a file bliss wrote itself.
        assert_eq!(store.save(&profile), Ok(None));
        assert_eq!(Store::new(path.clone()).save(&profile), Ok(None));
        assert_eq!(text(&backup), before);
    }

    #[test]
    fn an_existing_backup_is_never_overwritten() {
        let path = scratch("older-backup");
        std::fs::write(path.with_extension("toml.bak"), "older").expect("write");
        std::fs::write(&path, "# by hand\n").expect("write");

        let backup = Store::new(path.clone())
            .save(&Profile::default())
            .expect("saved")
            .expect("a backup was made");
        assert_eq!(backup, path.with_extension("toml.1.bak"));
        assert_eq!(text(&backup), "# by hand\n");
        assert_eq!(text(&path.with_extension("toml.bak")), "older");
    }

    /// Even with bliss's header: whatever is in a file that does not parse is
    /// the only copy of it.
    #[test]
    fn a_profile_that_does_not_parse_is_kept_too() {
        let path = scratch("broken");
        let before = format!("{HEADER}[[server]]\nlink = \"vless://unterminated\n");
        std::fs::write(&path, &before).expect("write");

        let backup = Store::new(path.clone())
            .save(&Profile::default())
            .expect("saved")
            .expect("a backup was made");
        assert_eq!(text(&backup), before);
        assert!(read(&path).expect("the new one parses").is_some());
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
        let mut profile = Profile::default();
        add_server(&mut profile, &link).expect("added");
        Store::new(path.clone()).save(&profile).expect("saved");
        assert_eq!(links(&read(&path).unwrap().unwrap()), [link.as_str()]);
    }

    #[test]
    fn the_same_server_twice_is_refused() {
        let mut profile = Profile::default();
        add_server(&mut profile, FIRST).expect("first time");
        let refused = add_server(&mut profile, &format!(" {FIRST} ")).expect_err("second time");
        assert!(refused.contains("уже есть"), "{refused}");
        assert_eq!(profile.servers.len(), 1);
    }

    #[test]
    fn a_refused_link_is_explained_without_being_repeated() {
        let mut profile = Profile::default();
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
            let refused = add_server(&mut profile, link).expect_err(link);
            assert!(refused.contains(expect), "{link}: {refused}");
            assert!(!refused.contains("SECRETTOKEN"), "{link}: {refused}");
        }
        assert!(profile.servers.is_empty(), "a refused link changes nothing");
    }

    #[test]
    fn the_starting_rules_go_in_once() {
        let mut profile = Profile::default();
        add_starting_rules(&mut profile).expect("the first time");
        assert_eq!(profile.routing.rules.len(), 2);
        assert_eq!(
            profile.routing.fallback,
            Action::Proxy,
            "the fallback is not theirs to change"
        );

        let before = profile.routing.rules.len();
        assert!(add_starting_rules(&mut profile).is_err());
        assert_eq!(profile.routing.rules.len(), before);
    }
}
