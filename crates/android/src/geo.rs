//! The rule sets on the phone: `geoip.dat` and `geosite.dat`, fetched by the
//! code the desktop shares (`rule_sets.rs`), and the phone's own settings
//! about them and about much else.

use std::path::Path;

use serde::{Deserialize, Serialize};

pub use crate::rule_sets::{
    MAX_FILE, MAX_NAMES, SOURCES, STALE, Source, ip_path, names, site_path, size, source, update,
};

/// The phone's own settings, beside the profile: the profile is the
/// desktop's format, which refuses keys it does not know.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub geo_source: String,
    /// Fetch again by the week, once the files are here.
    pub geo_auto: bool,
    /// When the files were last fetched, in seconds since 1970.
    pub geo_updated: Option<i64>,
    /// The switch 0.2.5 had for refusing QUIC, read once to give the
    /// built-in rule that replaced it the same position (`quic_rule`).
    pub block_quic: bool,
    /// Whether the built-in «QUIC» rule has been offered to this profile:
    /// once, so a person who deletes it is not given it back.
    pub quic_rule: bool,
    /// «Автовыбор»: the fastest server that answers, and the next one when
    /// it stops (engine.rs, `auto_pick`).
    pub auto_server: bool,
    /// What bliss does on each network (networks.rs).
    pub networks: Vec<crate::networks::NetworkRule>,
    /// The servers marked with a star, by key (`protocol::server_key`),
    /// oldest first; the list puts them above the rest.
    pub favorites: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            geo_source: SOURCES[0].key.to_owned(),
            geo_auto: true,
            geo_updated: None,
            block_quic: true,
            quic_rule: false,
            auto_server: false,
            networks: Vec::new(),
            favorites: Vec::new(),
        }
    }
}

impl Settings {
    pub fn load(files: &Path) -> Self {
        std::fs::read_to_string(files.join("android.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, files: &Path) {
        let path = files.join("android.json");
        let tmp = path.with_extension("json.new");
        let written = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|text| std::fs::write(&tmp, text))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = written {
            tracing::error!("geo: the settings were not saved: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_survive_a_round_trip_and_an_unknown_source_is_the_first() {
        let dir = std::env::temp_dir().join(format!("bliss-geo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(Settings::load(&dir).geo_source, "loyalsoldier");
        let settings = Settings {
            geo_source: "v2fly".to_owned(),
            geo_auto: false,
            geo_updated: Some(42),
            block_quic: false,
            quic_rule: true,
            auto_server: true,
            networks: Vec::new(),
            favorites: Vec::new(),
        };
        settings.save(&dir);
        let back = Settings::load(&dir);
        assert_eq!(back.geo_source, "v2fly");
        assert!(!back.block_quic);
        assert!(!back.geo_auto);
        assert_eq!(back.geo_updated, Some(42));
        assert!(back.auto_server);
        assert_eq!(source("nonsense").key, "loyalsoldier");
        // A file from before the QUIC switch reads with it on.
        std::fs::write(dir.join("android.json"), r#"{"geo_source":"v2fly"}"#).unwrap();
        assert!(Settings::load(&dir).block_quic);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
