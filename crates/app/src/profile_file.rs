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

use bliss_proto::{
    Action, LinkError, Profile, Rule, ServerConfig, ServerEntry, Subscription, Unsupported,
};

use crate::subscription::Fetched;
use crate::ui::plural;

/// The most servers a profile holds, pasted and subscribed together. Every one
/// is a row in a frame published twice a second and a socket in every sweep,
/// and a subscription can bring thousands.
pub const MAX_PROFILE_SERVERS: usize = 5_000;

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
        return Err("вставьте ссылку на сервер".to_owned());
    }
    let config: ServerConfig = link.parse().map_err(|e| link_problem(link, &e))?;
    if profile.servers.iter().any(|s| s.link.trim() == link) {
        return Err(format!("этот сервер уже есть: «{}»", config.tag));
    }
    if profile.servers.len() >= MAX_PROFILE_SERVERS {
        return Err(full());
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
            geosite: list(&["category-ads-all"]),
            ..Rule::default()
        },
        Rule {
            name: Some("Локальная сеть".to_owned()),
            action: Action::Direct,
            ip: list(&[
                "127.0.0.0/8",
                "10.0.0.0/8",
                "172.16.0.0/12",
                "192.168.0.0/16",
            ]),
            ..Rule::default()
        },
    ]
}

fn full() -> String {
    format!(
        "в профиле уже {} — больше bliss не держит",
        plural(MAX_PROFILE_SERVERS as u64, "сервер", "сервера", "серверов")
    )
}

/// What pasting servers did.
#[derive(Debug)]
pub struct Imported {
    pub added: Vec<ServerConfig>,
    /// Already in the profile.
    pub known: usize,
    /// Links to protocols xray-core does not speak, and links that do not parse.
    pub skipped: usize,
}

impl Imported {
    /// One line for the window.
    pub fn summary(&self) -> String {
        let mut text = match self.added.as_slice() {
            [one] => format!("Добавлен «{}»", one.tag),
            many => format!(
                "Добавлено {}",
                plural(many.len() as u64, "сервер", "сервера", "серверов")
            ),
        };
        if self.known > 0 {
            text.push_str(&format!(", ещё {} уже были", self.known));
        }
        if self.skipped > 0 {
            text.push_str(&format!(", {} пропущено", self.skipped));
        }
        text
    }
}

/// Add every server in a pasted text: one link, several, or a base64 list.
///
/// A single line that is not a working link is explained the way `add_server`
/// explains it; a list is added as far as it goes.
pub fn import_servers(profile: &mut Profile, text: &str) -> Result<Imported, String> {
    let text = text.trim();
    let found = crate::subscription::links_in(text);
    if found.links.is_empty() {
        if !text.contains('\n') {
            return add_server(profile, text).map(|config| Imported {
                added: vec![config],
                known: 0,
                skipped: 0,
            });
        }
        return Err(match (found.foreign, found.broken) {
            (0, 0) => "в тексте нет ссылок на серверы".to_owned(),
            (foreign, 0) => format!(
                "{} на протоколы, которых нет в xray-core",
                plural(foreign as u64, "ссылка", "ссылки", "ссылок")
            ),
            (_, broken) => format!(
                "{} не читаются — скопируйте заново",
                plural(broken as u64, "ссылка", "ссылки", "ссылок")
            ),
        });
    }

    let mut imported = Imported {
        added: Vec::new(),
        known: 0,
        skipped: found.foreign + found.broken,
    };
    for link in found.links {
        if profile.servers.iter().any(|s| s.link.trim() == link) {
            imported.known += 1;
            continue;
        }
        let Ok(config) = link.parse::<ServerConfig>() else {
            imported.skipped += 1;
            continue;
        };
        if profile.servers.len() >= MAX_PROFILE_SERVERS {
            if imported.added.is_empty() {
                return Err(full());
            }
            break;
        }
        profile.servers.push(ServerEntry {
            link,
            name: None,
            group: None,
        });
        imported.added.push(config);
    }
    if imported.added.is_empty() {
        return Err("все эти серверы уже есть".to_owned());
    }
    Ok(imported)
}

// --- rules --------------------------------------------------------------------

/// Put a rule in the profile: in place of the one at `at`, or after the others.
pub fn save_rule(profile: &mut Profile, at: Option<usize>, rule: Rule) -> Result<(), String> {
    if rule.is_empty() {
        return Err("у правила нет ни одного условия".to_owned());
    }
    let rules = &mut profile.routing.rules;
    match at {
        // The editor knows the rule's words, not its id or its switch: an
        // edited rule stays the same rule to every preset, and stays off if
        // it was.
        Some(at) => {
            let old = rules.get_mut(at).ok_or_else(no_such_rule)?;
            *old = Rule {
                id: std::mem::take(&mut old.id),
                enabled: old.enabled,
                ..rule
            };
        }
        None => rules.push(Rule {
            id: String::new(),
            enabled: true,
            ..rule
        }),
    }
    Ok(())
}

pub fn delete_rule(profile: &mut Profile, at: usize) -> Result<Rule, String> {
    let rules = &mut profile.routing.rules;
    if at >= rules.len() {
        return Err(no_such_rule());
    }
    let rule = rules.remove(at);
    profile.routing.forget(&rule.id);
    Ok(rule)
}

/// Switch a rule on or off. It keeps its place, and a preset in force stays
/// named: the window says it has changed.
pub fn switch_rule(profile: &mut Profile, at: usize, on: bool) -> Result<(), String> {
    profile
        .routing
        .rules
        .get_mut(at)
        .ok_or_else(no_such_rule)?
        .enabled = on;
    Ok(())
}

// --- presets ------------------------------------------------------------------

/// The most presets a profile holds. Every one is a pill on two screens and a
/// line in the tray's menu, which has room for four before it scrolls.
pub const MAX_PRESETS: usize = 24;

/// The longest name a preset takes; the pills elide nothing.
pub const MAX_PRESET_NAME: usize = 32;

fn preset_name(name: &str) -> Result<String, String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("у пресета нет названия".to_owned());
    }
    if name.chars().count() > MAX_PRESET_NAME {
        return Err(format!(
            "название длиннее {} — сократите его",
            plural(MAX_PRESET_NAME as u64, "символа", "символов", "символов")
        ));
    }
    Ok(name)
}

fn no_such_preset() -> String {
    "этого пресета уже нет — список успел измениться".to_owned()
}

/// Keep where every switch stands now, under a name: a new preset, or the one
/// by that name updated. True when it is new. Either way it becomes the
/// preset in force.
pub fn save_preset(profile: &mut Profile, name: &str) -> Result<bool, String> {
    let name = preset_name(name)?;
    let routing = &mut profile.routing;
    let snapshot = routing.snapshot(&name);
    let created = match routing.presets.iter_mut().find(|p| p.name == name) {
        Some(known) => {
            *known = snapshot;
            false
        }
        None => {
            if routing.presets.len() >= MAX_PRESETS {
                return Err(format!(
                    "пресетов уже {MAX_PRESETS} — больше bliss не держит; удалите ненужный"
                ));
            }
            routing.presets.push(snapshot);
            true
        }
    };
    routing.current_preset = Some(name);
    Ok(created)
}

/// Put every switch where a preset has it.
pub fn apply_preset(profile: &mut Profile, name: &str) -> Result<(), String> {
    if profile.routing.apply(name) {
        Ok(())
    } else {
        Err(no_such_preset())
    }
}

pub fn rename_preset(profile: &mut Profile, from: &str, to: &str) -> Result<String, String> {
    let to = preset_name(to)?;
    let routing = &mut profile.routing;
    if to != from && routing.preset(&to).is_some() {
        return Err(format!("пресет «{to}» уже есть"));
    }
    routing
        .presets
        .iter_mut()
        .find(|p| p.name == from)
        .ok_or_else(no_such_preset)?
        .name = to.clone();
    if routing.current_preset.as_deref() == Some(from) {
        routing.current_preset = Some(to.clone());
    }
    Ok(to)
}

/// Delete a preset. The switches stay where they are.
pub fn delete_preset(profile: &mut Profile, name: &str) -> Result<(), String> {
    let routing = &mut profile.routing;
    let at = routing
        .presets
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(no_such_preset)?;
    routing.presets.remove(at);
    if routing.current_preset.as_deref() == Some(name) {
        routing.current_preset = None;
    }
    Ok(())
}

/// Move a rule to another place in the order — another priority, since the
/// first rule that matches decides.
pub fn move_rule(profile: &mut Profile, from: usize, to: usize) -> Result<(), String> {
    let rules = &mut profile.routing.rules;
    if from >= rules.len() || to >= rules.len() {
        return Err(no_such_rule());
    }
    let rule = rules.remove(from);
    rules.insert(to, rule);
    Ok(())
}

fn no_such_rule() -> String {
    "этого правила уже нет — список успел измениться".to_owned()
}

/// What a rule made from the connections table is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Subject {
    /// A program, by the name Windows gives it: `chrome.exe`.
    Program(String),
    /// A site and everything under it: `googlevideo.com`.
    Site(String),
    /// One address, for a connection that named no site.
    Address(String),
}

impl Subject {
    pub fn value(&self) -> &str {
        match self {
            Self::Program(value) | Self::Site(value) | Self::Address(value) => value,
        }
    }
}

/// Send one program, site or address somewhere, from the connections table.
///
/// A rule about this and nothing else just changes its action. Otherwise a new
/// rule goes after the others, so every rule already there still decides
/// first: a program sent direct keeps its advertising blocked. Returns where
/// the rule is.
pub fn route_one(profile: &mut Profile, subject: &Subject, action: Action) -> usize {
    let rules = &mut profile.routing.rules;
    let only_this = |rule: &Rule| {
        let conditions = rule.domain.len()
            + rule.ip.len()
            + rule.port.len()
            + rule.process.len()
            + rule.geosite.len()
            + rule.geoip.len()
            + rule.network.len();
        let list = match subject {
            Subject::Program(_) => &rule.process,
            Subject::Site(_) => &rule.domain,
            Subject::Address(_) => &rule.ip,
        };
        conditions == 1
            && list
                .first()
                .is_some_and(|value| value.eq_ignore_ascii_case(subject.value()))
    };
    if let Some(at) = rules.iter().position(only_this) {
        rules[at].action = action;
        return at;
    }
    let mut rule = Rule {
        action,
        ..Rule::default()
    };
    match subject {
        Subject::Program(name) => rule.process.push(name.clone()),
        Subject::Site(site) => rule.domain.push(site.clone()),
        Subject::Address(ip) => rule.ip.push(ip.clone()),
    }
    rules.push(rule);
    rules.len() - 1
}

// --- subscriptions ------------------------------------------------------------

/// The text as a subscription's address: one http(s) address and nothing else.
pub fn subscription_address(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let url = reqwest::Url::parse(text).ok()?;
    let web = matches!(url.scheme(), "http" | "https");
    (web && url.host_str().is_some_and(|host| !host.is_empty())).then(|| text.to_owned())
}

/// Add a subscription. Its servers come with the first fetch.
///
/// `seed` makes the id; the engine passes the time, and a taken id moves on.
pub fn add_subscription(
    profile: &mut Profile,
    url: &str,
    seed: u64,
) -> Result<Subscription, String> {
    if let Some(known) = profile.subscriptions.iter().find(|s| s.url == url) {
        return Err(format!("эта подписка уже есть: «{}»", known.name));
    }
    let id = (seed..)
        .map(|n| format!("{n:x}"))
        .find(|id| !profile.subscriptions.iter().any(|s| &s.id == id))
        .expect("some id is free");
    let subscription = Subscription {
        id,
        name: host_name(url),
        url: url.to_owned(),
        updated: None,
        usage: None,
        renamed: false,
    };
    profile.subscriptions.push(subscription.clone());
    Ok(subscription)
}

/// What a subscription is called before its provider says: the address's
/// host, which is the part of it that is not secret.
fn host_name(url: &str) -> String {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "подписка".to_owned())
}

/// What an edit changed.
#[derive(Debug, PartialEq, Eq)]
pub struct Edited {
    /// What it is called now.
    pub name: String,
    /// What it was called, when that changed.
    pub renamed_from: Option<String>,
    /// The address changed: until a list from the new one arrives, its
    /// servers are the old one's.
    pub moved: bool,
    /// The name was handed back to the provider, whose title comes with the
    /// next list.
    pub unnamed: bool,
}

/// Change a subscription's name, its address, or both.
///
/// A name given here is kept: the provider's title, which replaced the name
/// at every update, no longer does. An empty one hands the naming back — the
/// address's host until the provider's title arrives. A new address keeps
/// the servers until a list from it replaces them, and drops what the old
/// one said about traffic and dates; `None`, or the same address, leaves it.
pub fn edit_subscription(
    profile: &mut Profile,
    id: &str,
    name: &str,
    url: Option<&str>,
) -> Result<Edited, String> {
    let at = profile
        .subscriptions
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| "этой подписки уже нет".to_owned())?;
    let url = match url.map(str::trim).filter(|url| !url.is_empty()) {
        None => None,
        Some(text) => {
            let url = subscription_address(text).ok_or_else(|| {
                "это не адрес подписки: нужен один адрес, начинающийся с http:// или https://"
                    .to_owned()
            })?;
            if let Some(other) = profile
                .subscriptions
                .iter()
                .find(|s| s.url == url && s.id != id)
            {
                return Err(format!("этот адрес уже у подписки «{}»", other.name));
            }
            (url != profile.subscriptions[at].url).then_some(url)
        }
    };
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");

    let subscription = &mut profile.subscriptions[at];
    let before = subscription.name.clone();
    let moved = url.is_some();
    if let Some(url) = url {
        subscription.url = url;
        subscription.updated = None;
        subscription.usage = None;
    }
    let unnamed = name.is_empty();
    if unnamed {
        subscription.renamed = false;
        subscription.name = host_name(&subscription.url);
    } else if name != subscription.name {
        subscription.renamed = true;
        subscription.name = name;
    }
    Ok(Edited {
        name: subscription.name.clone(),
        renamed_from: (subscription.name != before).then_some(before),
        moved,
        unnamed,
    })
}

/// What an update did.
#[derive(Debug, PartialEq, Eq)]
pub struct Refreshed {
    pub name: String,
    pub servers: usize,
    pub added: usize,
    pub removed: usize,
}

/// Put a fetched list in the profile.
///
/// The servers the subscription brought last time are replaced, in the same
/// place in the list; servers added by hand are not touched, and a link the
/// profile already holds is not added twice.
pub fn apply_subscription(
    profile: &mut Profile,
    id: &str,
    fetched: &Fetched,
    now: i64,
) -> Result<Refreshed, String> {
    let at = profile
        .subscriptions
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| "этой подписки уже нет".to_owned())?;
    let group = Some(id.to_owned());
    let before: Vec<String> = profile
        .servers
        .iter()
        .filter(|s| s.group == group)
        .map(|s| s.link.clone())
        .collect();
    // Everything before the first of them is not theirs, so after they are
    // taken out this is still where they began.
    let place = profile.servers.iter().position(|s| s.group == group);
    profile.servers.retain(|s| s.group != group);

    let room = MAX_PROFILE_SERVERS.saturating_sub(profile.servers.len());
    let fresh: Vec<ServerEntry> = fetched
        .links
        .links
        .iter()
        .filter(|link| {
            !profile
                .servers
                .iter()
                .any(|s| s.link.trim() == link.as_str())
        })
        .take(room)
        .map(|link| ServerEntry {
            link: link.clone(),
            name: None,
            group: group.clone(),
        })
        .collect();
    let refreshed = |name: String| Refreshed {
        name,
        servers: fresh.len(),
        added: fresh.iter().filter(|e| !before.contains(&e.link)).count(),
        removed: before
            .iter()
            .filter(|link| !fresh.iter().any(|e| &e.link == *link))
            .count(),
    };

    let subscription = &mut profile.subscriptions[at];
    // The provider's title, unless the window gave it a name of its own.
    if let Some(title) = fetched.title.as_ref().filter(|_| !subscription.renamed) {
        subscription.name = title.clone();
    }
    if fetched.usage.is_some() {
        subscription.usage = fetched.usage;
    }
    subscription.updated = Some(now);
    let result = refreshed(subscription.name.clone());

    let place = place.unwrap_or(profile.servers.len());
    profile.servers.splice(place..place, fresh);
    Ok(result)
}

/// Remove a subscription and every server it brought.
pub fn delete_subscription(profile: &mut Profile, id: &str) -> Result<Subscription, String> {
    let at = profile
        .subscriptions
        .iter()
        .position(|s| s.id == id)
        .ok_or_else(|| "этой подписки уже нет".to_owned())?;
    let group = Some(id.to_owned());
    profile.servers.retain(|s| s.group != group);
    Ok(profile.subscriptions.remove(at))
}

/// Remove one server, found by its link.
///
/// One that came from a subscription is back with the subscription's next
/// update, the way it is in v2rayN; the window says so when it happens.
pub fn delete_server(profile: &mut Profile, link: &str) -> Result<ServerEntry, String> {
    let at = profile
        .servers
        .iter()
        .position(|s| s.link == link)
        .ok_or_else(|| "этого сервера уже нет".to_owned())?;
    Ok(profile.servers.remove(at))
}

/// Why a pasted link was refused, in words for the window.
///
/// By variant rather than from the error's text, which is English and meant
/// for the log. And never with the link in it: a value that failed to parse
/// can be most of a key.
fn link_problem(link: &str, e: &LinkError) -> String {
    match e {
        LinkError::Scheme(scheme) if scheme == "https" || scheme == "http" => {
            "это адрес страницы, а не ссылка на сервер. Адрес подписки добавьте в разделе \
             «Подписки»"
                .to_owned()
        }
        LinkError::Scheme(scheme) => format!(
            "{scheme}:// — не ссылка на сервер. bliss понимает vless, vmess, trojan, ss, socks, \
             hysteria2 и wireguard"
        ),
        LinkError::NotInXray(scheme) => {
            format!("это сервер {scheme}, а такой протокол движок xray-core не поддерживает")
        }
        LinkError::Url(_) if !link.contains("://") => {
            "это не похоже на ссылку — она начинается с vless://, trojan:// и тому подобного"
                .to_owned()
        }
        LinkError::Url(_) => "ссылка повреждена — скопируйте её заново целиком".to_owned(),
        LinkError::MissingUuid | LinkError::Uuid(_) => {
            "в ссылке нет правильного UUID пользователя — скопируйте её заново целиком".to_owned()
        }
        LinkError::MissingHost => "в ссылке нет адреса сервера".to_owned(),
        LinkError::MissingPort => "в ссылке нет порта сервера".to_owned(),
        LinkError::Missing(what) => {
            let what = match *what {
                "password" => "пароля",
                "method and password" => "шифра и пароля",
                "private key" => "закрытого ключа",
                "publickey" => "ключа сервера (publickey)",
                "obfs-password" => "пароля обфускации (obfs-password)",
                other => other,
            };
            format!("в ссылке нет {what} — скопируйте её заново целиком")
        }
        LinkError::RealityMissing(param) => {
            format!("для REALITY в ссылке не хватает параметра {param}")
        }
        LinkError::BadValue {
            param, expected, ..
        } => {
            format!("в ссылке неверный параметр {param}: ожидается {expected}")
        }
        LinkError::UnsupportedTransport(transport) => {
            format!(
                "транспорта {transport} в xray-core больше нет — попросите у провайдера новую ссылку"
            )
        }
        LinkError::Unsupported(what) => match what {
            Unsupported::Cipher(cipher) => format!(
                "шифр {cipher} устарел, и xray-core его больше не поддерживает — \
                 попросите у провайдера ссылку с AEAD-шифром"
            ),
            Unsupported::Plugin(plugin) => format!(
                "плагин {plugin} xray-core заменить не может — нужен сам плагин, а bliss их не запускает"
            ),
            Unsupported::Obfs(obfs) => {
                format!("обфускацию {obfs} xray-core не умеет — только salamander")
            }
            Unsupported::LegacyXtls => {
                "старый XTLS (security=xtls) убран из xray-core — нужна ссылка с Vision".to_owned()
            }
        },
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
            ("tuic://SECRETTOKEN@a.example:443", "xray-core"),
            // A cipher xray dropped is named; the password beside it is not.
            (
                "ss://YWVzLTI1Ni1jZmI6U0VDUkVUVE9LRU4@a.example:8388",
                "aes-256-cfb",
            ),
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

    fn server_link(host: &str) -> String {
        format!("vless://11111111-2222-3333-4444-555555555555@{host}:443?encryption=none#{host}")
    }

    fn fetched(links: &[String], title: Option<&str>) -> Fetched {
        Fetched {
            links: crate::subscription::links_in(&links.join("\n")),
            title: title.map(str::to_owned),
            usage: Some(bliss_proto::Usage {
                upload: 1,
                download: 2,
                total: 0,
                expire: None,
            }),
        }
    }

    #[test]
    fn pasted_text_adds_every_server_in_it() {
        use base64::Engine as _;
        let mut profile = Profile::default();
        add_server(&mut profile, FIRST).expect("added");

        let list = format!(
            "{FIRST}\n{SECOND}\nvmess://AAAA\n{}",
            server_link("c.example")
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(&list);
        let imported = import_servers(&mut profile, &encoded).expect("imported");
        assert_eq!(imported.added.len(), 2);
        assert_eq!((imported.known, imported.skipped), (1, 1));
        assert_eq!(
            imported.summary(),
            "Добавлено 2 сервера, ещё 1 уже были, 1 пропущено"
        );
        assert_eq!(profile.servers.len(), 3);

        let again = import_servers(&mut profile, &list).expect_err("nothing new");
        assert!(again.contains("уже есть"), "{again}");
        // One line keeps the precise reason a single link gets.
        let one = import_servers(&mut profile, "tuic://SECRETTOKEN@a.example:443")
            .expect_err("not xray's");
        assert!(
            one.contains("tuic") && !one.contains("SECRETTOKEN"),
            "{one}"
        );
    }

    /// The cap on what a profile holds, whichever way servers arrive.
    #[test]
    fn a_profile_holds_at_most_max_profile_servers() {
        let mut profile = Profile {
            servers: (0..MAX_PROFILE_SERVERS)
                .map(|n| ServerEntry {
                    link: server_link(&format!("h{n}.example")),
                    name: None,
                    group: None,
                })
                .collect(),
            ..Profile::default()
        };
        assert!(add_server(&mut profile, FIRST).is_err());
        assert!(import_servers(&mut profile, &format!("{FIRST}\n{SECOND}")).is_err());

        let subscription =
            add_subscription(&mut profile, "https://sub.example/x", 1).expect("added");
        let refreshed = apply_subscription(
            &mut profile,
            &subscription.id,
            &fetched(&[server_link("new.example")], None),
            5,
        )
        .expect("applied");
        assert_eq!(refreshed.servers, 0);
        assert_eq!(profile.servers.len(), MAX_PROFILE_SERVERS);
    }

    #[test]
    fn rules_are_saved_moved_and_deleted_in_place() {
        let mut profile = Profile::default();
        add_starting_rules(&mut profile).expect("rules");
        let mine = crate::rule_text::parse("мои", Action::Proxy, "geoip:ru").expect("reads");
        save_rule(&mut profile, None, mine.clone()).expect("appended");
        assert_eq!(profile.routing.rules[2], mine);

        move_rule(&mut profile, 2, 0).expect("moved");
        assert_eq!(profile.routing.rules[0], mine);
        assert_eq!(
            profile.routing.rules[1].name.as_deref(),
            Some("Реклама и трекеры")
        );

        let mut changed = mine.clone();
        changed.action = Action::Direct;
        save_rule(&mut profile, Some(0), changed).expect("replaced");
        assert_eq!(profile.routing.rules[0].action, Action::Direct);
        assert_eq!(profile.routing.rules.len(), 3);

        assert_eq!(delete_rule(&mut profile, 0).expect("deleted").geoip, ["ru"]);
        assert!(delete_rule(&mut profile, 7).is_err());
        assert!(move_rule(&mut profile, 0, 9).is_err());
        assert!(save_rule(&mut profile, Some(9), mine).is_err());
    }

    /// From the connections table: the second choice for the same program
    /// changes the first rule instead of stacking another under it.
    #[test]
    fn a_quick_rule_goes_last_and_is_reused() {
        let mut profile = Profile::default();
        add_starting_rules(&mut profile).expect("rules");
        let chrome = Subject::Program("chrome.exe".to_owned());

        assert_eq!(route_one(&mut profile, &chrome, Action::Direct), 2);
        assert_eq!(profile.routing.rules[2].process, ["chrome.exe"]);
        assert_eq!(
            route_one(
                &mut profile,
                &Subject::Program("CHROME.EXE".to_owned()),
                Action::Proxy
            ),
            2
        );
        assert_eq!(profile.routing.rules.len(), 3);
        assert_eq!(profile.routing.rules[2].action, Action::Proxy);

        // A rule that also says something else is not "only this program".
        profile.routing.rules[2].domain.push("a.example".to_owned());
        assert_eq!(route_one(&mut profile, &chrome, Action::Block), 3);
        let site = Subject::Site("googlevideo.com".to_owned());
        assert_eq!(route_one(&mut profile, &site, Action::Direct), 4);
        assert_eq!(profile.routing.rules[4].domain, ["googlevideo.com"]);
    }

    #[test]
    fn a_server_is_deleted_by_its_link_and_only_that_one() {
        let mut profile = Profile::default();
        add_server(&mut profile, FIRST).expect("added");
        add_server(&mut profile, SECOND).expect("added");
        assert_eq!(
            delete_server(&mut profile, FIRST).expect("deleted").link,
            FIRST
        );
        assert_eq!(links(&profile), [SECOND]);
        assert!(delete_server(&mut profile, FIRST).is_err(), "gone already");
        assert_eq!(links(&profile), [SECOND]);
    }

    #[test]
    fn a_subscription_replaces_only_its_own_servers() {
        let mut profile = Profile::default();
        add_server(&mut profile, FIRST).expect("by hand");
        assert_eq!(
            subscription_address(" https://sub.example/api/x "),
            Some("https://sub.example/api/x".to_owned())
        );
        assert_eq!(subscription_address(FIRST), None);
        assert_eq!(subscription_address("https://a b"), None);

        let url = "https://sub.example/api/sub/TOKEN";
        let subscription = add_subscription(&mut profile, url, 0xabc).expect("added");
        assert_eq!(subscription.name, "sub.example");
        assert_eq!(subscription.id, "abc");
        let twice = add_subscription(&mut profile, url, 1).expect_err("once");
        assert!(twice.contains("уже есть"), "{twice}");
        add_server(&mut profile, SECOND).expect("by hand, after");

        let first = [
            server_link("s1.example"),
            server_link("s2.example"),
            FIRST.to_owned(),
        ];
        let refreshed = apply_subscription(
            &mut profile,
            "abc",
            &fetched(&first, Some("Провайдер")),
            100,
        )
        .expect("applied");
        assert_eq!(
            refreshed,
            Refreshed {
                name: "Провайдер".to_owned(),
                servers: 2,
                added: 2,
                removed: 0
            },
            "a link already in the profile is not added twice"
        );
        // Its servers sit where the subscription was added: after the end.
        let groups: Vec<Option<&str>> =
            profile.servers.iter().map(|s| s.group.as_deref()).collect();
        assert_eq!(groups, [None, None, Some("abc"), Some("abc")]);

        // The list changes: one server gone, one new, in the same place.
        profile.servers.push(ServerEntry {
            link: server_link("mine.example"),
            name: None,
            group: None,
        });
        let second = [server_link("s2.example"), server_link("s3.example")];
        let refreshed =
            apply_subscription(&mut profile, "abc", &fetched(&second, None), 200).expect("applied");
        assert_eq!((refreshed.added, refreshed.removed), (1, 1));
        assert_eq!(
            refreshed.name, "Провайдер",
            "a list without a title keeps the name"
        );
        let hosts: Vec<&str> = profile
            .servers
            .iter()
            .map(|s| s.link.split('#').nth(1).unwrap_or_default())
            .collect();
        assert_eq!(
            hosts,
            [
                "first",
                "second",
                "s2.example",
                "s3.example",
                "mine.example"
            ]
        );
        assert_eq!(profile.subscriptions[0].updated, Some(200));

        delete_subscription(&mut profile, "abc").expect("deleted");
        assert!(profile.subscriptions.is_empty());
        assert_eq!(
            profile.servers.len(),
            3,
            "its servers go with it, and only those"
        );
        assert!(apply_subscription(&mut profile, "abc", &fetched(&second, None), 300).is_err());
    }

    /// A name given in the window outlives the provider's title, and an empty
    /// one hands the naming back. A new address keeps the servers until its
    /// list comes; one another subscription has, or that is no address at
    /// all, is refused with nothing changed.
    #[test]
    fn a_subscription_is_renamed_and_moved_in_place() {
        let mut profile = Profile::default();
        let first = add_subscription(&mut profile, "https://sub.example/api/sub/TOKEN", 0xa)
            .expect("added");
        add_subscription(&mut profile, "https://other.example/x", 0xb).expect("added");
        let list = [server_link("s1.example")];
        apply_subscription(
            &mut profile,
            &first.id,
            &fetched(&list, Some("Провайдер")),
            100,
        )
        .expect("applied");

        let edited =
            edit_subscription(&mut profile, "a", "  Мой   провайдер ", None).expect("renamed");
        assert_eq!(
            edited,
            Edited {
                name: "Мой провайдер".to_owned(),
                renamed_from: Some("Провайдер".to_owned()),
                moved: false,
                unnamed: false,
            }
        );
        let refreshed =
            apply_subscription(&mut profile, "a", &fetched(&list, Some("Провайдер")), 200)
                .expect("applied");
        assert_eq!(
            refreshed.name, "Мой провайдер",
            "the provider's title no longer replaces it"
        );
        let same = edit_subscription(&mut profile, "a", "Мой провайдер", None).expect("kept");
        assert_eq!(same.renamed_from, None, "the same name is no rename");

        let moved = edit_subscription(
            &mut profile,
            "a",
            "Мой провайдер",
            Some(" https://new.example/sub/NEWTOKEN "),
        )
        .expect("moved");
        assert!(moved.moved);
        let subscription = &profile.subscriptions[0];
        assert_eq!(subscription.url, "https://new.example/sub/NEWTOKEN");
        assert_eq!((subscription.updated, subscription.usage), (None, None));
        assert_eq!(
            profile
                .servers
                .iter()
                .filter(|s| s.group.as_deref() == Some("a"))
                .count(),
            1,
            "its servers wait for the new address's list"
        );
        let again = edit_subscription(
            &mut profile,
            "a",
            "Мой провайдер",
            Some("https://new.example/sub/NEWTOKEN"),
        )
        .expect("the same address");
        assert!(!again.moved);

        let taken = edit_subscription(&mut profile, "a", "x", Some("https://other.example/x"))
            .expect_err("another subscription's");
        assert!(taken.contains("other.example"), "{taken}");
        assert!(edit_subscription(&mut profile, "a", "x", Some(FIRST)).is_err());
        assert!(edit_subscription(&mut profile, "zz", "x", None).is_err());
        assert_eq!(profile.subscriptions[0].name, "Мой провайдер");

        let unnamed = edit_subscription(&mut profile, "a", "  ", None).expect("unnamed");
        assert!(unnamed.unnamed);
        assert_eq!(
            unnamed.name, "new.example",
            "the host, until the title comes"
        );
        let refreshed =
            apply_subscription(&mut profile, "a", &fetched(&list, Some("Провайдер")), 300)
                .expect("applied");
        assert_eq!(refreshed.name, "Провайдер");
    }

    /// Subscriptions, usage and groups have to survive the whole-file rewrite.
    #[test]
    fn a_profile_with_a_subscription_reads_back() {
        let path = scratch("subscription");
        let mut profile = Profile::default();
        let subscription =
            add_subscription(&mut profile, "https://sub.example/api/TOKEN", 7).expect("added");
        add_subscription(&mut profile, "https://other.example/api/TOKEN", 8).expect("added");
        apply_subscription(
            &mut profile,
            &subscription.id,
            &fetched(&[server_link("s1.example")], Some("Провайдер")),
            1_700_000_000,
        )
        .expect("applied");
        edit_subscription(&mut profile, &subscription.id, "Свой", None).expect("renamed");
        Store::new(path.clone()).save(&profile).expect("saved");

        let text = std::fs::read_to_string(&path).expect("written");
        assert_eq!(
            text.matches("renamed = true").count(),
            1,
            "written for the one renamed, and only for it"
        );
        let back = read(&path).expect("parses").expect("exists");
        assert_eq!(back.subscriptions, profile.subscriptions);
        assert_eq!(
            back.servers[0].group.as_deref(),
            Some(subscription.id.as_str())
        );
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

#[cfg(test)]
mod preset_tests {
    use super::*;

    fn profile_with(names: &[&str]) -> Profile {
        let mut profile = Profile::default();
        for name in names {
            save_rule(
                &mut profile,
                None,
                Rule {
                    name: Some((*name).to_owned()),
                    action: Action::Direct,
                    domain: vec![format!("{name}.example")],
                    ..Rule::default()
                },
            )
            .expect("a rule with a condition saves");
        }
        profile.routing.ensure_ids();
        profile
    }

    /// The editor sends words; the rule it edits keeps its id, so every
    /// preset still knows it, and stays off if it was off.
    #[test]
    fn an_edited_rule_keeps_its_id_and_its_switch() {
        let mut profile = profile_with(&["a", "b"]);
        switch_rule(&mut profile, 1, false).expect("switched");
        let id = profile.routing.rules[1].id.clone();
        save_rule(
            &mut profile,
            Some(1),
            Rule {
                name: Some("b2".to_owned()),
                action: Action::Block,
                domain: vec!["b2.example".to_owned()],
                ..Rule::default()
            },
        )
        .expect("edited");
        let edited = &profile.routing.rules[1];
        assert_eq!((edited.id.as_str(), edited.enabled), (id.as_str(), false));
        assert_eq!(edited.action, Action::Block);
    }

    #[test]
    fn a_deleted_rule_leaves_every_preset() {
        let mut profile = profile_with(&["a", "b"]);
        save_preset(&mut profile, "Дом").expect("saved");
        delete_rule(&mut profile, 0).expect("deleted");
        let preset = &profile.routing.presets[0];
        assert_eq!(preset.on, [profile.routing.rules[0].id.clone()]);
    }

    #[test]
    fn a_preset_is_made_updated_renamed_and_deleted() {
        let mut profile = profile_with(&["a", "b"]);
        assert_eq!(save_preset(&mut profile, "  Игры  "), Ok(true));
        assert_eq!(profile.routing.current_preset.as_deref(), Some("Игры"));

        switch_rule(&mut profile, 0, false).expect("switched");
        assert!(profile.routing.drifted());
        // Saved again under the same name: updated, not a second one.
        assert_eq!(save_preset(&mut profile, "Игры"), Ok(false));
        assert_eq!(profile.routing.presets.len(), 1);
        assert!(!profile.routing.drifted());

        save_preset(&mut profile, "Работа").expect("second");
        assert!(
            rename_preset(&mut profile, "Работа", "Игры").is_err(),
            "names are unique"
        );
        assert_eq!(
            rename_preset(&mut profile, "Работа", "Офис"),
            Ok("Офис".to_owned())
        );
        assert_eq!(profile.routing.current_preset.as_deref(), Some("Офис"));

        apply_preset(&mut profile, "Игры").expect("applied");
        assert!(!profile.routing.rules[0].enabled);
        delete_preset(&mut profile, "Игры").expect("deleted");
        assert_eq!(profile.routing.current_preset, None);
        assert!(
            !profile.routing.rules[0].enabled,
            "the switches stay where they are"
        );
        assert!(apply_preset(&mut profile, "Игры").is_err());

        assert!(save_preset(&mut profile, "   ").is_err());
        assert!(save_preset(&mut profile, &"я".repeat(MAX_PRESET_NAME + 1)).is_err());
    }

    /// Every preset is a pill on two screens and a line in the tray.
    #[test]
    fn presets_have_a_ceiling() {
        let mut profile = profile_with(&["a"]);
        for n in 0..MAX_PRESETS {
            save_preset(&mut profile, &format!("p{n}")).expect("under the cap");
        }
        let refused = save_preset(&mut profile, "one more").expect_err("the cap holds");
        assert!(refused.contains("удалите"), "{refused}");
        // Updating one that exists is not one more.
        assert_eq!(save_preset(&mut profile, "p0"), Ok(false));
        assert_eq!(profile.routing.presets.len(), MAX_PRESETS);
    }
}
