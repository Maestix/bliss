//! Networks: bliss follows the phone (docs/android-design.md).
//!
//! Every network can say whether bliss connects on it, stays off, or leaves
//! it to the heart, and which preset applies there. A network is known by
//! what Android tells the tunnel's process (`TunnelService`): mobile data,
//! a Wi-Fi by its name when the person allowed bliss to read it — Android
//! gives an app the SSID only with location permission — or a Wi-Fi with
//! no name. A Wi-Fi without a rule of its own takes the rule for «любой
//! Wi-Fi», if there is one.

use serde::{Deserialize, Serialize};

/// How many networks can have a rule: anything that grows has a cap
/// (AGENTS.md §5), and the oldest rule goes first past it.
pub const MAX_NETWORKS: usize = 32;

/// The phone's network as the service reports it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Seen {
    /// "mobile", "wifi", or another transport Android names ("ethernet").
    pub kind: String,
    /// The Wi-Fi's name, when bliss may read it.
    #[serde(default)]
    pub ssid: Option<String>,
    /// Behind a captive portal: the network wants a sign-in first.
    #[serde(default)]
    pub captive: bool,
}

impl Seen {
    /// The key a rule for exactly this network has.
    pub fn id(&self) -> String {
        match (&*self.kind, &self.ssid) {
            ("wifi", Some(ssid)) => format!("wifi:{ssid}"),
            (kind, _) => kind.to_owned(),
        }
    }

    /// What a person calls it.
    pub fn name(&self) -> String {
        match (&*self.kind, &self.ssid) {
            ("wifi", Some(ssid)) => ssid.clone(),
            ("wifi", None) => "Wi-Fi".to_owned(),
            ("mobile", _) => "Мобильная сеть".to_owned(),
            ("ethernet", _) => "Кабель".to_owned(),
            (kind, _) => kind.to_owned(),
        }
    }
}

/// What bliss does on one network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRule {
    /// "mobile", "wifi" (any Wi-Fi), "wifi:<name>".
    pub id: String,
    /// "on" connect here, "off" stay off here, "" leave it to the heart.
    #[serde(default)]
    pub connect: String,
    /// The preset to apply here; none leaves the rules as they are.
    #[serde(default)]
    pub preset: Option<String>,
}

/// The rule that speaks for `seen`: its own, else the one for any Wi-Fi.
pub fn rule_for<'a>(rules: &'a [NetworkRule], seen: &Seen) -> Option<&'a NetworkRule> {
    let id = seen.id();
    rules.iter().find(|r| r.id == id).or_else(|| {
        (seen.kind == "wifi")
            .then(|| rules.iter().find(|r| r.id == "wifi"))
            .flatten()
    })
}

/// Write `rule` in place of the one with its id, or add it; a rule that
/// says nothing (no connect, no preset) is taken away instead.
pub fn put(rules: &mut Vec<NetworkRule>, rule: NetworkRule) {
    rules.retain(|r| r.id != rule.id);
    if rule.connect.is_empty() && rule.preset.is_none() {
        return;
    }
    rules.push(rule);
    while rules.len() > MAX_NETWORKS {
        rules.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi(name: Option<&str>) -> Seen {
        Seen {
            kind: "wifi".to_owned(),
            ssid: name.map(str::to_owned),
            captive: false,
        }
    }

    #[test]
    fn a_wifi_of_its_own_comes_before_any_wifi() {
        let rules = vec![
            NetworkRule {
                id: "wifi".to_owned(),
                connect: "on".to_owned(),
                preset: None,
            },
            NetworkRule {
                id: "wifi:Дом".to_owned(),
                connect: "off".to_owned(),
                preset: Some("Дома".to_owned()),
            },
        ];
        assert_eq!(rule_for(&rules, &wifi(Some("Дом"))).unwrap().connect, "off");
        assert_eq!(rule_for(&rules, &wifi(Some("Кафе"))).unwrap().connect, "on");
        assert_eq!(rule_for(&rules, &wifi(None)).unwrap().connect, "on");
        let mobile = Seen {
            kind: "mobile".to_owned(),
            ssid: None,
            captive: false,
        };
        assert!(rule_for(&rules, &mobile).is_none());
        assert_eq!(wifi(Some("Дом")).id(), "wifi:Дом");
    }

    #[test]
    fn rules_have_a_cap_and_an_empty_one_is_taken_away() {
        let mut rules = Vec::new();
        for n in 0..MAX_NETWORKS + 5 {
            put(
                &mut rules,
                NetworkRule {
                    id: format!("wifi:n{n}"),
                    connect: "on".to_owned(),
                    preset: None,
                },
            );
        }
        assert_eq!(rules.len(), MAX_NETWORKS);
        assert_eq!(rules[0].id, "wifi:n5", "the oldest go first");
        put(
            &mut rules,
            NetworkRule {
                id: "wifi:n6".to_owned(),
                connect: String::new(),
                preset: None,
            },
        );
        assert!(!rules.iter().any(|r| r.id == "wifi:n6"));
    }
}
