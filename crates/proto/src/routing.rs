//! Routing rules, as the user writes them.
//!
//! Three things can happen to a connection, and the whole feature is those
//! three words: send it through the proxy, let it go straight out, or refuse
//! it. Everything else here is about deciding which.
//!
//! Rules are read top to bottom and the first match wins. That is the one
//! ordering rule worth having, because it is the only one a person can hold in
//! their head — "put the exceptions above the general case" needs no further
//! explanation.

use serde::{Deserialize, Serialize};

/// What to do with a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Through the tunnel.
    #[default]
    Proxy,
    /// Straight out, not through the tunnel. Faster, and visible to anyone
    /// watching the local network — which is the point for a site that is
    /// local anyway.
    Direct,
    /// Refused. Nothing is sent anywhere; the program asking is told no.
    Block,
}

impl Action {
    /// The word the window uses.
    pub fn label(self) -> &'static str {
        match self {
            Self::Proxy => "через прокси",
            Self::Direct => "напрямую",
            Self::Block => "блокировать",
        }
    }
}

/// Everything routing needs from the profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Routing {
    /// What happens to a connection no rule matched.
    ///
    /// `proxy` by default, which is both the safe answer — nothing leaks by
    /// omission — and what a client with no rules at all does today.
    #[serde(default)]
    pub fallback: Action,

    /// Where the geo databases live. Unset means the copies that came with the
    /// engine, in bliss's own folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_dir: Option<std::path::PathBuf>,

    /// The preset applied last, by name: the one the window marks, and says
    /// has changed once a switch has moved since. Before the lists, because a
    /// plain key cannot follow a table in TOML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_preset: Option<String>,

    // Left out when empty, like every list in a rule: bliss writes the profile
    // itself, and `rule = []` is noise in a file that has no rules.
    #[serde(default, rename = "rule", skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,

    /// Named positions of the rules' switches, to put them all back at once.
    #[serde(default, rename = "preset", skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<Preset>,
}

impl Default for Routing {
    fn default() -> Self {
        Self {
            fallback: Action::Proxy,
            geo_dir: None,
            current_preset: None,
            rules: Vec::new(),
            presets: Vec::new(),
        }
    }
}

/// Where every switch stood when a preset was saved, and where everything
/// else went.
///
/// Rules are named by id, so a rule keeps its place in every preset however it
/// is moved, renamed or edited. A rule in neither list was made after the
/// preset: applying it leaves that rule as it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub name: String,
    #[serde(default)]
    pub fallback: Action,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub off: Vec<String>,
}

impl Routing {
    /// Give every rule an id that names it and nothing else, ever: one more
    /// than any id a rule or a preset holds, so a deleted rule's id is not
    /// handed to the next rule while a preset may still say where it stood.
    pub fn ensure_ids(&mut self) {
        let mut next = self
            .rules
            .iter()
            .map(|rule| rule.id.as_str())
            .chain(
                self.presets
                    .iter()
                    .flat_map(|p| p.on.iter().chain(&p.off).map(String::as_str)),
            )
            .filter_map(|id| id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        let mut seen = std::collections::HashSet::new();
        for rule in &mut self.rules {
            // A copied rule carries its original's id; the copy gets its own.
            if rule.id.is_empty() || !seen.insert(rule.id.clone()) {
                next += 1;
                rule.id = next.to_string();
                seen.insert(rule.id.clone());
            }
        }
    }

    pub fn preset(&self, name: &str) -> Option<&Preset> {
        self.presets.iter().find(|preset| preset.name == name)
    }

    /// The switches as they stand now, under `name`. Ids first.
    pub fn snapshot(&mut self, name: &str) -> Preset {
        self.ensure_ids();
        let (on, off): (Vec<&Rule>, Vec<&Rule>) = self.rules.iter().partition(|rule| rule.enabled);
        Preset {
            name: name.to_owned(),
            fallback: self.fallback,
            on: on.into_iter().map(|rule| rule.id.clone()).collect(),
            off: off.into_iter().map(|rule| rule.id.clone()).collect(),
        }
    }

    /// Put every switch where the preset has it. False when there is no
    /// preset by that name.
    pub fn apply(&mut self, name: &str) -> bool {
        let Some(preset) = self.preset(name).cloned() else {
            return false;
        };
        for rule in &mut self.rules {
            if preset.on.contains(&rule.id) {
                rule.enabled = true;
            } else if preset.off.contains(&rule.id) {
                rule.enabled = false;
            }
        }
        self.fallback = preset.fallback;
        self.current_preset = Some(preset.name);
        true
    }

    /// Whether a switch, or where everything else goes, has moved since the
    /// current preset was applied or saved.
    pub fn drifted(&self) -> bool {
        let Some(preset) = self
            .current_preset
            .as_deref()
            .and_then(|name| self.preset(name))
        else {
            return false;
        };
        preset.fallback != self.fallback
            || self.rules.iter().any(|rule| {
                (rule.enabled && preset.off.contains(&rule.id))
                    || (!rule.enabled && preset.on.contains(&rule.id))
            })
    }

    /// Forget a rule's id everywhere, once the rule is gone.
    pub fn forget(&mut self, id: &str) {
        for preset in &mut self.presets {
            preset.on.retain(|known| known != id);
            preset.off.retain(|known| known != id);
        }
    }
}

fn on() -> bool {
    true
}

fn is_on(enabled: &bool) -> bool {
    *enabled
}

/// One rule: what to match, and what to do about it.
///
/// Every list is optional and a rule matches if **any** entry in **any** of its
/// lists matches. Written the other way round — all lists having to agree — the
/// common case ("these twenty domains go direct") would need twenty rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// What to call this rule in the log and on screen.
    ///
    /// Worth filling in: every connection bliss handles records the rule that
    /// decided it, and "Реклама" tells the user something that
    /// `geosite:category-ads-all` does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// What presets call it. Given by bliss (`Routing::ensure_ids`), never
    /// shown, never changed; a rule written by hand gets one on the next save.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,

    /// Off, the rule decides nothing and loads nothing, and keeps its place.
    /// Written to the profile only when off.
    #[serde(default = "on", skip_serializing_if = "is_on")]
    pub enabled: bool,

    pub action: Action,

    /// Domain patterns. Bare text means "this name and everything under it";
    /// `full:` means exactly this name; `keyword:` means the text appears
    /// anywhere in the name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain: Vec<String>,

    /// Addresses and blocks, as `10.0.0.0/8` or a single `1.2.3.4`.
    ///
    /// Only ever matches a connection that named an address rather than a name
    /// — see the note on [`Rule::geoip`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ip: Vec<String>,

    /// Destination ports: `443`, or `1000-2000`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub port: Vec<String>,

    /// Programs, by executable name: `qbittorrent.exe`. Case-insensitive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub process: Vec<String>,

    /// Categories from `geosite.dat`: `category-ads-all`, `ru`, `google`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geosite: Vec<String>,

    /// Countries from `geoip.dat`: `ru`, `private`.
    ///
    /// **These match addresses, not names.** A browser asks for
    /// `example.ru` and bliss deliberately never resolves it — that is what
    /// keeps the DNS lookup off the local network — so there is no address to
    /// compare. Use `geosite` to route sites by country; `geoip` catches the
    /// programs that connect to a bare address. Both become useful together in
    /// TUN mode, where bliss sees packets rather than requests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub geoip: Vec<String>,
}

/// A rule that sends nothing anywhere yet: switched on, conditions to fill in.
impl Default for Rule {
    fn default() -> Self {
        Self {
            name: None,
            id: String::new(),
            enabled: true,
            action: Action::default(),
            domain: Vec::new(),
            ip: Vec::new(),
            port: Vec::new(),
            process: Vec::new(),
            geosite: Vec::new(),
            geoip: Vec::new(),
        }
    }
}

impl Rule {
    /// True if the rule would never match anything.
    ///
    /// A rule with an action and no conditions reads as "everything", which is
    /// almost certainly not what someone meant to write — so it is reported
    /// rather than applied.
    pub fn is_empty(&self) -> bool {
        self.domain.is_empty()
            && self.ip.is_empty()
            && self.port.is_empty()
            && self.process.is_empty()
            && self.geosite.is_empty()
            && self.geoip.is_empty()
    }

    /// The name to show, invented from the contents if none was given.
    pub fn display_name(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let mut parts = Vec::new();
        let mut describe = |label: &str, list: &[String]| {
            if let Some(first) = list.first() {
                parts.push(match list.len() {
                    1 => format!("{label} {first}"),
                    n => format!("{label} {first} +{}", n - 1),
                });
            }
        };
        describe("домен", &self.domain);
        describe("набор", &self.geosite);
        describe("адрес", &self.ip);
        describe("страна", &self.geoip);
        describe("порт", &self.port);
        describe("процесс", &self.process);
        if parts.is_empty() {
            return "пустое правило".to_owned();
        }
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str) -> Rule {
        Rule {
            name: Some(name.to_owned()),
            domain: vec![format!("{name}.example")],
            ..Rule::default()
        }
    }

    fn routing(names: &[&str]) -> Routing {
        let mut routing = Routing {
            rules: names.iter().map(|name| rule(name)).collect(),
            ..Routing::default()
        };
        routing.ensure_ids();
        routing
    }

    /// An id names one rule for good: a preset that still says where a
    /// deleted rule stood must not come to mean the next rule made.
    #[test]
    fn an_id_is_never_handed_out_twice() {
        let mut r = routing(&["a", "b", "c"]);
        assert_eq!(
            r.rules
                .iter()
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            ["1", "2", "3"]
        );
        let preset = r.snapshot("all");
        r.presets.push(preset);
        r.rules.pop();
        r.rules.push(rule("d"));
        r.ensure_ids();
        assert_eq!(r.rules[2].id, "4", "3 is still named by the preset");

        // A rule copied with its id gets one of its own.
        let copy = r.rules[0].clone();
        r.rules.push(copy);
        r.ensure_ids();
        assert_eq!(r.rules[3].id, "5");
    }

    #[test]
    fn a_preset_puts_every_switch_back() {
        let mut r = routing(&["a", "b", "c"]);
        r.rules[1].enabled = false;
        r.fallback = Action::Direct;
        let games = r.snapshot("Игры");
        r.presets.push(games);
        r.current_preset = Some("Игры".to_owned());
        assert!(!r.drifted());

        // Moved by hand: the preset is still named, and has drifted.
        r.rules[0].enabled = false;
        r.rules[1].enabled = true;
        assert!(r.drifted());
        // A rule made since is none of the preset's business.
        r.rules.push(rule("new"));
        r.ensure_ids();
        r.rules[3].enabled = false;

        assert!(r.apply("Игры"));
        let on: Vec<bool> = r.rules.iter().map(|rule| rule.enabled).collect();
        assert_eq!(on, [true, false, true, false]);
        assert_eq!(r.fallback, Action::Direct);
        assert!(!r.drifted());

        // Where everything else goes is part of it too.
        r.fallback = Action::Proxy;
        assert!(r.drifted());
        assert!(!r.apply("нет такого"));
    }

    #[test]
    fn forgetting_a_rule_takes_it_out_of_every_preset() {
        let mut r = routing(&["a", "b"]);
        let both = r.snapshot("оба");
        r.presets.push(both);
        r.forget("1");
        assert_eq!(r.presets[0].on, ["2"]);
    }

    /// The profile says `enabled = false` for a rule that is off and nothing
    /// for one that is on, and reads a rule written before switches existed
    /// as on.
    #[test]
    fn switches_and_presets_survive_the_profile() {
        let mut r = routing(&["a", "b"]);
        r.rules[1].enabled = false;
        let preset = r.snapshot("Дом");
        r.presets.push(preset);
        r.current_preset = Some("Дом".to_owned());

        let text = toml::to_string_pretty(&r).expect("routing serialises");
        assert_eq!(text.matches("enabled").count(), 1, "{text}");
        assert!(text.contains("enabled = false"), "{text}");
        let back: Routing = toml::from_str(&text).expect("and reads back");
        assert_eq!(back.rules, r.rules);
        assert_eq!(back.presets, r.presets);
        assert_eq!(back.current_preset.as_deref(), Some("Дом"));

        let old: Routing =
            toml::from_str("[[rule]]\naction = \"direct\"\ndomain = [\"a.example\"]\n")
                .expect("an old rule reads");
        assert!(old.rules[0].enabled);
        assert!(old.rules[0].id.is_empty());
    }
}
