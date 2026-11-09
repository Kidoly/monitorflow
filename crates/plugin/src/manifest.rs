//! What a plugin is and asks for (ADR-0103): its name, version and kind,
//! the hosts it may call and the settings it takes. Everything here is
//! checked before a package is signed, again before it is installed, and
//! shown to the administrator in plain words.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::http::HostRule;

/// The ABI this build runs.
pub const ABI: u32 = 1;
/// Most settings of one plugin.
pub const MAX_SETTINGS: usize = 20;
/// Most hosts in `capabilities.http`.
pub const MAX_HOSTS: usize = 16;
/// Longest setting value.
pub const MAX_VALUE_LEN: usize = 2048;
/// The check parameter that names the plugin, so no setting may take it.
pub const PLUGIN_PARAM: &str = "plugin";
/// The sensor kind of check plugins in a sensor configuration.
pub const CHECK_KIND: &str = "plugin";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ManifestError(pub String);

fn invalid(message: impl Into<String>) -> ManifestError {
    ManifestError(message.into())
}

/// What a plugin does, and who runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A check the agent runs.
    Check,
    /// A notification channel the central server runs.
    Channel,
    /// A hook for outgoing events (ADR-0102), not run yet.
    Integration,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Channel => "channel",
            Self::Integration => "integration",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingType {
    Text,
    Url,
    Number,
    Bool,
    /// Never shown again once saved; only channels have them in v1.
    Secret,
}

/// One field of the plugin's settings form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setting {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: SettingType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Hosts the plugin may send HTTP requests to: `name`, `name:port` or
    /// `{settings.KEY}` for the host of a `url` setting.
    #[serde(default)]
    pub http: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub name: String,
    pub version: String,
    pub kind: Kind,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub abi: u32,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub settings: Vec<Setting>,
}

/// A `MAJOR.MINOR.PATCH` version, compared by number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.').map(|part| {
            let digits =
                !part.is_empty() && part.len() <= 9 && part.bytes().all(|b| b.is_ascii_digit());
            let canonical = part == "0" || !part.starts_with('0');
            (digits && canonical).then(|| part.parse().ok()).flatten()
        });
        let version = Self(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

fn plain_text(value: &str, max: usize) -> bool {
    value.chars().count() <= max && !value.chars().any(char::is_control)
}

/// `a-z`, digits and `-`, 1 to 40, starting with a letter.
pub fn valid_name(name: &str) -> bool {
    name.len() <= 40
        && name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && !name.ends_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_key(key: &str) -> bool {
    key.len() <= 32
        && key.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

impl Manifest {
    /// Parses and checks the manifest's text.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        if text.len() > crate::limits::MANIFEST_BYTES {
            return Err(invalid("the manifest is too large"));
        }
        let manifest: Self =
            serde_json::from_str(text).map_err(|e| invalid(format!("manifest: {e}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if !valid_name(&self.name) {
            return Err(invalid(
                "name: 1 to 40 lowercase letters, digits and dashes, starting with a letter",
            ));
        }
        if Version::parse(&self.version).is_none() {
            return Err(invalid("version: MAJOR.MINOR.PATCH, like 1.0.0"));
        }
        if self.abi != ABI {
            return Err(invalid(format!(
                "abi: this version of MonitorFlow runs ABI {ABI}, not {}",
                self.abi
            )));
        }
        if self.kind == Kind::Integration {
            return Err(invalid("integration plugins are not supported yet"));
        }
        if self.title.trim().is_empty() || !plain_text(&self.title, 60) {
            return Err(invalid("title: 1 to 60 characters"));
        }
        if !plain_text(&self.description.replace('\n', " "), 500) {
            return Err(invalid("description: at most 500 characters"));
        }
        if self.settings.len() > MAX_SETTINGS {
            return Err(invalid(format!("at most {MAX_SETTINGS} settings")));
        }
        for (i, setting) in self.settings.iter().enumerate() {
            self.validate_setting(setting)?;
            if self.settings[..i].iter().any(|s| s.key == setting.key) {
                return Err(invalid(format!("setting {}: listed twice", setting.key)));
            }
        }
        if self.capabilities.http.len() > MAX_HOSTS {
            return Err(invalid(format!(
                "capabilities.http: at most {MAX_HOSTS} hosts"
            )));
        }
        for host in &self.capabilities.http {
            match setting_reference(host) {
                Some(key) => {
                    let url = self
                        .setting(key)
                        .is_some_and(|s| s.kind == SettingType::Url);
                    if !url {
                        return Err(invalid(format!(
                            "capabilities.http: {host} names no url setting"
                        )));
                    }
                }
                None => {
                    HostRule::parse(host)
                        .map_err(|e| invalid(format!("capabilities.http: {e}")))?;
                }
            }
        }
        Ok(())
    }

    fn validate_setting(&self, setting: &Setting) -> Result<(), ManifestError> {
        let key = &setting.key;
        if !valid_key(key) || key == PLUGIN_PARAM {
            return Err(invalid(format!(
                "setting {key}: keys are 1 to 32 of a-z, digits and _, and not `{PLUGIN_PARAM}`"
            )));
        }
        if setting.label.trim().is_empty() || !plain_text(&setting.label, 60) {
            return Err(invalid(format!(
                "setting {key}: label of 1 to 60 characters"
            )));
        }
        if setting.help.as_deref().is_some_and(|h| !plain_text(h, 300)) {
            return Err(invalid(format!(
                "setting {key}: help of at most 300 characters"
            )));
        }
        if setting.kind == SettingType::Secret {
            if self.kind == Kind::Check {
                return Err(invalid(format!(
                    "setting {key}: checks cannot have secret settings yet"
                )));
            }
            if setting.default.is_some() {
                return Err(invalid(format!("setting {key}: a secret has no default")));
            }
        }
        if let Some(default) = &setting.default {
            check_value(setting, default)
                .map_err(|e| invalid(format!("setting {key}: default: {e}")))?;
        }
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Option<&Setting> {
        self.settings.iter().find(|s| s.key == key)
    }

    pub fn version(&self) -> Version {
        Version::parse(&self.version).unwrap_or(Version(0, 0, 0))
    }

    /// Whether any setting is secret.
    pub fn has_secrets(&self) -> bool {
        self.settings.iter().any(|s| s.kind == SettingType::Secret)
    }

    /// `values` checked against the settings, with defaults filled in,
    /// split into plain settings and secret ones. Unknown keys, missing
    /// required values and values of the wrong type are refused; an empty
    /// value counts as missing.
    pub fn values(&self, values: &BTreeMap<String, String>) -> Result<Values, ManifestError> {
        if let Some(key) = values.keys().find(|k| self.setting(k).is_none()) {
            return Err(invalid(format!("{key}: not a setting of {}", self.name)));
        }
        let mut out = Values::default();
        for setting in &self.settings {
            let value = values
                .get(&setting.key)
                .filter(|v| !v.is_empty())
                .or(setting.default.as_ref());
            let Some(value) = value else {
                if setting.required {
                    return Err(invalid(format!("{} is required", setting.label)));
                }
                continue;
            };
            check_value(setting, value).map_err(|e| invalid(format!("{}: {e}", setting.label)))?;
            let map = if setting.kind == SettingType::Secret {
                &mut out.secrets
            } else {
                &mut out.settings
            };
            map.insert(setting.key.clone(), value.clone());
        }
        Ok(out)
    }

    /// The hosts a run with `settings` may call. A reference to a setting
    /// without a value allows nothing.
    pub fn host_rules(&self, settings: &BTreeMap<String, String>) -> Vec<HostRule> {
        self.capabilities
            .http
            .iter()
            .filter_map(|host| match setting_reference(host) {
                Some(key) => HostRule::of_url(settings.get(key)?),
                None => HostRule::parse(host).ok(),
            })
            .collect()
    }

    /// What the plugin does and asks for, in the words the web interface
    /// shows before it is installed.
    pub fn capabilities_in_words(&self) -> Vec<String> {
        let mut words = vec![match self.kind {
            Kind::Check => "Runs on your servers as a check".to_owned(),
            Kind::Channel => "Sends notifications from the central server".to_owned(),
            Kind::Integration => "Receives events from the central server".to_owned(),
        }];
        if self.capabilities.http.is_empty() {
            words.push("Connects to nothing".to_owned());
        }
        for host in &self.capabilities.http {
            words.push(
                match setting_reference(host).and_then(|k| self.setting(k)) {
                    Some(setting) => {
                        format!("Connects to the address in its {} setting", setting.label)
                    }
                    None => format!("Connects to {host}"),
                },
            );
        }
        if self.has_secrets() {
            words.push("Can read the secret settings you give it".to_owned());
        }
        words
    }
}

/// The key of a `{settings.KEY}` host.
fn setting_reference(host: &str) -> Option<&str> {
    host.strip_prefix("{settings.")?.strip_suffix('}')
}

fn check_value(setting: &Setting, value: &str) -> Result<(), String> {
    if !plain_text(value, MAX_VALUE_LEN) {
        return Err(format!("at most {MAX_VALUE_LEN} characters, on one line"));
    }
    match setting.kind {
        SettingType::Text | SettingType::Secret => Ok(()),
        SettingType::Number => value
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .map(drop)
            .ok_or_else(|| "a number".to_owned()),
        SettingType::Bool => matches!(value, "true" | "false")
            .then_some(())
            .ok_or_else(|| "true or false".to_owned()),
        SettingType::Url => HostRule::of_url(value)
            .map(drop)
            .ok_or_else(|| "an http or https address without a user name or password".to_owned()),
    }
}

/// Settings checked by [`Manifest::values`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Values {
    pub settings: BTreeMap<String, String>,
    pub secrets: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn ntfy() -> &'static str {
        r#"{
            "name": "ntfy", "version": "1.2.0", "kind": "channel", "title": "ntfy",
            "description": "Sends notifications to an ntfy topic.", "abi": 1,
            "capabilities": {"http": ["{settings.server}", "api.example.com:8443"]},
            "settings": [
                {"key": "server", "label": "Server", "type": "url", "required": true, "default": "https://ntfy.sh"},
                {"key": "topic", "label": "Topic", "type": "text", "required": true},
                {"key": "priority", "label": "Priority", "type": "number"},
                {"key": "token", "label": "Access token", "type": "secret"}
            ]
        }"#
    }

    fn with(change: impl FnOnce(&mut serde_json::Value)) -> Result<Manifest, ManifestError> {
        let mut value: serde_json::Value = serde_json::from_str(ntfy()).unwrap();
        change(&mut value);
        Manifest::parse(&value.to_string())
    }

    #[test]
    fn a_valid_manifest_says_what_it_asks_for() {
        let manifest = Manifest::parse(ntfy()).unwrap();
        assert_eq!(manifest.version(), Version(1, 2, 0));
        assert_eq!(
            manifest.capabilities_in_words(),
            [
                "Sends notifications from the central server",
                "Connects to the address in its Server setting",
                "Connects to api.example.com:8443",
                "Can read the secret settings you give it",
            ]
        );
    }

    #[test]
    fn bad_manifests_say_why() {
        type Change = fn(&mut serde_json::Value);
        let cases: [(&str, Change); 11] = [
            ("name:", |v| v["name"] = "Ntfy".into()),
            ("version:", |v| v["version"] = "1.02.0".into()),
            ("abi:", |v| v["abi"] = 2.into()),
            ("not supported yet", |v| v["kind"] = "integration".into()),
            ("title:", |v| v["title"] = "".into()),
            ("names no url setting", |v| {
                v["capabilities"]["http"][0] = "{settings.topic}".into()
            }),
            ("capabilities.http:", |v| {
                v["capabilities"]["http"][0] = "user@host".into()
            }),
            ("listed twice", |v| {
                v["settings"][1]["key"] = "server".into()
            }),
            ("checks cannot have secret", |v| v["kind"] = "check".into()),
            ("not `plugin`", |v| {
                v["settings"][1]["key"] = "plugin".into()
            }),
            ("default:", |v| {
                v["settings"][0]["default"] = "ftp://x".into()
            }),
        ];
        for (expected, change) in cases {
            let error = with(change).unwrap_err().0;
            assert!(error.contains(expected), "{expected}: {error}");
        }
        assert!(Manifest::parse(r#"{"name": "x"}"#).is_err());
        assert!(with(|v| v["unknown"] = 1.into()).is_err());
    }

    #[test]
    fn values_are_checked_and_secrets_kept_apart() {
        let manifest = Manifest::parse(ntfy()).unwrap();
        let values = |pairs: &[(&str, &str)]| {
            let map = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            manifest.values(&map)
        };
        let ok = values(&[("topic", "alerts"), ("token", "tk_1")]).unwrap();
        assert_eq!(ok.settings["server"], "https://ntfy.sh");
        assert_eq!(ok.settings["topic"], "alerts");
        assert_eq!(ok.secrets["token"], "tk_1");
        assert!(!ok.settings.contains_key("token"));
        assert!(values(&[]).unwrap_err().0.contains("Topic is required"));
        assert!(values(&[("topic", "")]).is_err());
        assert!(values(&[("topic", "a"), ("priority", "high")]).is_err());
        assert!(values(&[("topic", "a"), ("server", "https://u:p@x")]).is_err());
        assert!(values(&[("topic", "a"), ("other", "1")]).is_err());
        assert!(values(&[("topic", "a\nb")]).is_err());
    }

    #[test]
    fn hosts_come_from_the_manifest_and_url_settings() {
        let manifest = Manifest::parse(ntfy()).unwrap();
        let settings = [("server".to_owned(), "http://ntfy.lan:8080/x".to_owned())].into();
        let rules: Vec<String> = manifest
            .host_rules(&settings)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(rules, ["ntfy.lan:8080", "api.example.com:8443"]);
        assert_eq!(manifest.host_rules(&BTreeMap::new()).len(), 1);
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(Version::parse("1.10.0") > Version::parse("1.9.9"));
        for bad in ["1.0", "1.0.0.0", "01.0.0", "1.-1.0", "1.0.0-beta", ""] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert_eq!(Version::parse("0.0.0"), Some(Version(0, 0, 0)));
    }
}
