use directories::{ProjectDirs, UserDirs};
use localsend_rs::protocol::{DeviceInfo, Protocol};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

/// LocalSend's three receive-consent modes. Older GTK builds stored a bool;
/// the official migration maps `true` to all senders and `false` to favorites.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuickSaveMode {
    Off,
    On,
    #[default]
    Paired,
}

impl<'de> Deserialize<'de> for QuickSaveMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum NamedMode {
            Off,
            On,
            Paired,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum StoredMode {
            Legacy(bool),
            Named(NamedMode),
        }

        Ok(match StoredMode::deserialize(deserializer)? {
            StoredMode::Legacy(true) | StoredMode::Named(NamedMode::On) => Self::On,
            StoredMode::Legacy(false) | StoredMode::Named(NamedMode::Paired) => Self::Paired,
            StoredMode::Named(NamedMode::Off) => Self::Off,
        })
    }
}

/// A saved device identity. Discovery may update its address, never its identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteDevice {
    pub alias: String,
    pub fingerprint: String,
    pub address: String,
    pub port: u16,
    pub protocol: Protocol,
    #[serde(default)]
    pub custom_alias: bool,
}

/// A stable map key for certificate fingerprints from different discovery paths.
/// HTTP peers may use opaque identities, which must retain their original form.
pub fn fingerprint_key(fingerprint: &str) -> String {
    let compact: String = fingerprint.chars().filter(|c| *c != ':').collect();
    if compact.len() == 64 && compact.bytes().all(|c| c.is_ascii_hexdigit()) {
        compact.to_ascii_lowercase()
    } else {
        fingerprint.to_owned()
    }
}

pub fn same_fingerprint(left: &str, right: &str) -> bool {
    left.chars().any(|c| c != ':')
        && left
            .chars()
            .filter(|c| *c != ':')
            .map(|c| c.to_ascii_lowercase())
            .eq(right
                .chars()
                .filter(|c| *c != ':')
                .map(|c| c.to_ascii_lowercase()))
}

impl FavoriteDevice {
    pub fn from_peer(peer: &DeviceInfo, alias: &str) -> io::Result<Self> {
        let custom_alias = !alias.trim().is_empty() && alias.trim() != peer.alias;
        let result = Self {
            alias: if custom_alias {
                alias.trim().into()
            } else {
                peer.alias.clone()
            },
            fingerprint: peer.fingerprint.clone(),
            address: peer.ip.clone().unwrap_or_default(),
            port: peer.port,
            protocol: peer.protocol,
            custom_alias,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> io::Result<()> {
        let fingerprint: String = self.fingerprint.chars().filter(|c| *c != ':').collect();
        if self.alias.trim().is_empty()
            || self.alias.chars().count() > 80
            || self.address.parse::<std::net::IpAddr>().is_err()
            || self.port == 0
            || fingerprint.is_empty()
            || (self.protocol == Protocol::Https
                && (fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit())))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A favorite needs a name, a valid IP address, a port, and a device fingerprint.",
            ));
        }
        Ok(())
    }

    /// Keep HTTPS favorites pinned to HTTPS even if discovery advertises HTTP.
    pub fn observe(&mut self, peer: &DeviceInfo) -> bool {
        if !same_fingerprint(&self.fingerprint, &peer.fingerprint) || self.protocol != peer.protocol
        {
            return false;
        }
        let Some(address) = peer
            .ip
            .as_ref()
            .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
        else {
            return false;
        };
        if peer.port == 0 {
            return false;
        }
        let before = self.clone();
        self.address = address.clone();
        self.port = peer.port;
        if !self.custom_alias && !peer.alias.trim().is_empty() && peer.alias.chars().count() <= 80 {
            self.alias = peer.alias.clone();
        }
        *self != before
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub alias: String,
    pub language: crate::i18n::Locale,
    pub theme: u32,
    pub port: u16,
    pub save_dir: PathBuf,
    pub quick_save: QuickSaveMode,
    pub save_to_history: bool,
    pub animations: bool,
    pub favorites: Vec<FavoriteDevice>,
    pub receive_pin: Option<String>,
    pub send_mode: u32,
    pub tray_enabled: bool,
    pub close_to_tray: bool,
    pub autostart: bool,
    pub start_minimized: bool,
    pub share_auto_accept: bool,
    pub oled: bool,
    pub color_mode: crate::appearance::ColorMode,
    pub custom_color: String,
    pub compact_rail_narrow: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            alias: crate::alias_gen::generate_random_alias(),
            language: crate::i18n::Locale::System,
            theme: 0,
            port: 53317,
            save_dir: UserDirs::new()
                .map(|d| d.download_dir().unwrap_or(d.home_dir()).to_owned())
                .unwrap_or_else(|| PathBuf::from("downloads")),
            quick_save: QuickSaveMode::Paired,
            save_to_history: true,
            animations: true,
            favorites: Vec::new(),
            receive_pin: None,
            send_mode: 0,
            tray_enabled: false,
            close_to_tray: false,
            autostart: false,
            start_minimized: false,
            share_auto_accept: false,
            oled: false,
            color_mode: crate::appearance::ColorMode::LocalSend,
            custom_color: "#009688".into(),
            compact_rail_narrow: false,
        }
    }
}

impl Settings {
    pub fn directory() -> PathBuf {
        ProjectDirs::from("org", "localsend", "localsend-gtk")
            .expect("a user configuration directory is required")
            .config_dir()
            .to_owned()
    }

    pub fn load() -> io::Result<Self> {
        let path = Self::directory().join("settings.json");
        let mut result: Self = match std::fs::read(&path) {
            Ok(data) => serde_json::from_slice(&data).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e),
        };
        result.normalize_color_mode();
        result.validate()?;
        Ok(result)
    }

    fn normalize_color_mode(&mut self) {
        if self.oled || self.color_mode == crate::appearance::ColorMode::Oled {
            self.oled = true;
            self.color_mode = crate::appearance::ColorMode::Oled;
        }
    }

    pub fn validate(&self) -> io::Result<()> {
        if crate::appearance::Color::parse(&self.custom_color).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use a six-digit color such as #009688.",
            ));
        }
        if self.alias.trim().is_empty()
            || self.alias.chars().count() > 80
            || self.port == 0
            || self.theme > 2
            || self.send_mode > 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use a device name of 1–80 characters and a port of 1–65535.",
            ));
        }
        if self.receive_pin.as_ref().is_some_and(|pin| {
            pin.is_empty() || pin.len() > 12 || !pin.chars().all(|c| c.is_ascii_digit())
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Use a receive PIN of 1–12 digits.",
            ));
        }
        for (index, favorite) in self.favorites.iter().enumerate() {
            favorite.validate()?;
            if self.favorites[..index]
                .iter()
                .any(|other| same_fingerprint(&other.fingerprint, &favorite.fingerprint))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "A favorite device can only be saved once.",
                ));
            }
        }
        Ok(())
    }

    pub fn save(&self) -> io::Result<()> {
        let mut normalized = self.clone();
        normalized.normalize_color_mode();
        normalized.validate()?;
        atomic_write(
            &Self::directory().join("settings.json"),
            &serde_json::to_vec_pretty(&normalized)?,
        )
    }
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("Missing parent directory"))?,
    )?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_keys_match_across_discovery_formats_without_changing_opaque_ids() {
        let lowercase = "ab".repeat(32);
        let colon_separated = ["AB"; 32].join(":");
        assert_eq!(fingerprint_key(&lowercase.to_ascii_uppercase()), lowercase);
        assert_eq!(fingerprint_key(&colon_separated), lowercase);
        assert_eq!(fingerprint_key("Http-Peer:Identity"), "Http-Peer:Identity");
        assert_eq!(fingerprint_key(""), "");
    }

    fn peer() -> DeviceInfo {
        let mut peer = DeviceInfo::new("Bright Orange".into(), 53317, Protocol::Https);
        peer.fingerprint = "a1".repeat(32);
        peer.ip = Some("192.168.1.20".into());
        peer
    }

    #[test]
    fn older_settings_default_new_features_and_favorites_survive_disk_roundtrip() {
        let old: Settings = serde_json::from_str(r#"{"alias":"Old device","port":53317}"#).unwrap();
        old.validate().unwrap();
        assert!(old.favorites.is_empty());
        assert!(old.receive_pin.is_none());
        assert_eq!(old.send_mode, 0);
        assert!(!old.tray_enabled);
        assert!(!old.close_to_tray);
        assert!(!old.autostart);
        assert!(!old.start_minimized);
        assert!(!old.share_auto_accept);
        assert_eq!(old.quick_save, QuickSaveMode::Paired);
        assert!(old.save_to_history);
        assert!(!old.oled);
        assert_eq!(old.color_mode, crate::appearance::ColorMode::LocalSend);
        assert_eq!(old.custom_color, "#009688");
        assert_eq!(old.language, crate::i18n::Locale::System);
        let favorite = FavoriteDevice::from_peer(&peer(), "Kitchen tablet").unwrap();
        let settings = Settings {
            favorites: vec![favorite.clone()],
            receive_pin: Some("001234".into()),
            send_mode: 1,
            tray_enabled: true,
            close_to_tray: true,
            autostart: true,
            start_minimized: true,
            share_auto_accept: true,
            save_to_history: false,
            oled: true,
            color_mode: crate::appearance::ColorMode::Oled,
            custom_color: "#7357C8".into(),
            language: crate::i18n::Locale::ZhTw,
            ..old
        };
        settings.validate().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        atomic_write(&path, &serde_json::to_vec_pretty(&settings).unwrap()).unwrap();
        let restored: Settings = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        restored.validate().unwrap();
        assert_eq!(restored.favorites, vec![favorite]);
        assert_eq!(restored.receive_pin.as_deref(), Some("001234"));
        assert_eq!(restored.send_mode, 1);
        assert!(restored.tray_enabled);
        assert!(restored.close_to_tray);
        assert!(restored.autostart);
        assert!(restored.start_minimized);
        assert!(restored.share_auto_accept);
        assert!(!restored.save_to_history);
        assert!(restored.oled);
        assert_eq!(restored.color_mode, crate::appearance::ColorMode::Oled);
        assert_eq!(restored.custom_color, "#7357C8");
        assert_eq!(restored.language, crate::i18n::Locale::ZhTw);
    }

    #[test]
    fn legacy_quick_save_bool_migrates_to_the_official_three_state_mode() {
        let enabled: Settings =
            serde_json::from_str(r#"{"alias":"Old device","quick_save":true}"#).unwrap();
        assert_eq!(enabled.quick_save, QuickSaveMode::On);

        let disabled: Settings =
            serde_json::from_str(r#"{"alias":"Old device","quick_save":false}"#).unwrap();
        assert_eq!(disabled.quick_save, QuickSaveMode::Paired);

        for mode in [QuickSaveMode::Off, QuickSaveMode::On, QuickSaveMode::Paired] {
            let settings = Settings {
                quick_save: mode,
                ..Settings::default()
            };
            let restored: Settings =
                serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
            assert_eq!(restored.quick_save, mode);
        }
        let paired = serde_json::to_value(Settings::default()).unwrap();
        assert_eq!(paired["quick_save"], "paired");
        assert!(serde_json::from_str::<Settings>(
            r#"{"alias":"Old device","quick_save":"favorites"}"#
        )
        .is_err());
    }

    #[test]
    fn old_oled_preferences_keep_their_palette_and_invalid_colors_are_rejected() {
        let old: Settings =
            serde_json::from_str(r#"{"alias":"OLED device","theme":2,"oled":true}"#).unwrap();
        old.validate().unwrap();
        assert!(old.oled);
        assert_eq!(old.theme, 2);
        assert_eq!(old.color_mode, crate::appearance::ColorMode::LocalSend);
        let mut normalized = old.clone();
        normalized.normalize_color_mode();
        assert_eq!(normalized.color_mode, crate::appearance::ColorMode::Oled);
        let mut invalid = old;
        invalid.custom_color = "#bad".into();
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn color_modes_serialize_in_the_official_order_and_default_to_localsend() {
        let modes = [
            crate::appearance::ColorMode::System,
            crate::appearance::ColorMode::LocalSend,
            crate::appearance::ColorMode::Oled,
            crate::appearance::ColorMode::Yaru,
            crate::appearance::ColorMode::Custom,
        ];
        let names: Vec<_> = modes
            .iter()
            .map(|mode| serde_json::to_value(mode).unwrap())
            .collect();
        assert_eq!(
            names,
            ["system", "local_send", "oled", "yaru", "custom"].map(serde_json::Value::from)
        );
        assert_eq!(
            Settings::default().color_mode,
            crate::appearance::ColorMode::LocalSend
        );
    }

    #[test]
    fn favorites_follow_addresses_without_replacing_identity_or_custom_name() {
        let mut peer = peer();
        let mut favorite = FavoriteDevice::from_peer(&peer, "Kitchen tablet").unwrap();
        peer.ip = Some("2001:db8::20".into());
        peer.alias = "Renamed device".into();
        peer.port = 54321;
        peer.fingerprint = peer.fingerprint.to_uppercase();
        assert!(favorite.observe(&peer));
        assert_eq!(favorite.address, "2001:db8::20");
        assert_eq!(favorite.port, 54321);
        assert_eq!(favorite.alias, "Kitchen tablet");
        assert_eq!(favorite.fingerprint, "a1".repeat(32));
        let previous = favorite.clone();
        peer.fingerprint = "b2".repeat(32);
        peer.ip = Some("192.168.1.55".into());
        assert!(!favorite.observe(&peer));
        assert_eq!(favorite, previous);
        peer.fingerprint = favorite.fingerprint.clone();
        peer.protocol = Protocol::Http;
        assert!(!favorite.observe(&peer));
        assert_eq!(favorite, previous);
    }

    #[test]
    fn automatic_favorite_names_follow_discovery_and_invalid_entries_are_rejected() {
        let mut peer = peer();
        let mut favorite = FavoriteDevice::from_peer(&peer, "").unwrap();
        assert!(!favorite.custom_alias);
        peer.alias = "Renamed device".into();
        assert!(favorite.observe(&peer));
        assert_eq!(favorite.alias, peer.alias);
        let mut settings = Settings {
            favorites: vec![favorite.clone()],
            ..Settings::default()
        };
        settings.favorites.push(FavoriteDevice {
            fingerprint: favorite.fingerprint.to_uppercase(),
            ..favorite.clone()
        });
        assert!(settings.validate().is_err());
        settings.favorites.pop();
        settings.favorites[0].address = "not-an-ip".into();
        assert!(settings.validate().is_err());
        settings.favorites[0] = favorite;
        settings.favorites[0].fingerprint = "malformed".into();
        assert!(settings.validate().is_err());
        assert!(!same_fingerprint("", ""));
        assert!(!same_fingerprint(":", ""));
    }

    #[test]
    fn validates_receive_pin_and_send_mode() {
        let mut settings = Settings::default();
        for value in ["", "12 34", "1234567890123", "١٢٣٤"] {
            settings.receive_pin = Some(value.into());
            assert!(settings.validate().is_err(), "Invalid PIN: {value}");
        }
        settings.receive_pin = Some("0000".into());
        settings.validate().unwrap();
        settings.send_mode = 2;
        assert!(settings.validate().is_err());
    }
    #[test]
    fn rejects_invalid_settings_and_preserves_unicode() {
        let mut settings = Settings {
            alias: "青い Apple".into(),
            ..Settings::default()
        };
        let decoded: Settings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert_eq!(decoded.alias, settings.alias);
        assert!(!decoded.compact_rail_narrow);
        settings.compact_rail_narrow = true;
        let decoded_compact: Settings =
            serde_json::from_slice(&serde_json::to_vec(&settings).unwrap()).unwrap();
        assert!(decoded_compact.compact_rail_narrow);
        settings.compact_rail_narrow = false;
        settings.alias = " ".into();
        assert!(settings.validate().is_err());
        settings.alias = "Apple".into();
        settings.port = 0;
        assert!(settings.validate().is_err());
    }
    #[test]
    fn replaces_settings_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
