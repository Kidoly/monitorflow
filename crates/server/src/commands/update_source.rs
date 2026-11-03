//! Where a Windows server takes its updates from (ADR-0086), as the agent
//! reports it in its package reports and `servers.windows_update_source`
//! keeps it.

use monitorflow_common::proto::v1::windows_update_source::{
    Automatic as ProtoAutomatic, Origin as ProtoOrigin,
};
use monitorflow_common::proto::v1::WindowsUpdateSource;
use serde::{Deserialize, Serialize};

/// Longest URL kept.
pub const MAX_URL: usize = 512;
/// Longest deferral Windows accepts, in days.
pub const MAX_DEFERRAL_DAYS: u32 = 365;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Windows Update or Microsoft Update.
    Microsoft,
    Wsus,
    /// Policy turns WSUS on without naming a server.
    WsusWithoutUrl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Automatic {
    Off,
    /// Tells about updates before downloading them.
    Notify,
    /// Downloads, then waits to install.
    Download,
    /// Installs on a schedule.
    Install,
    /// Local administrators choose.
    LocalChoice,
}

/// The update source of a Windows server, as the API returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSource {
    pub origin: Origin,
    /// With `wsus`; with `microsoft`, a server policy names but does not
    /// turn on, so Windows ignores it.
    pub wsus_url: Option<String>,
    /// Where the server reports to, when not the WSUS server.
    pub status_url: Option<String>,
    /// Where downloads come from, when not the WSUS server.
    pub download_url: Option<String>,
    /// Any Windows Update policy is set.
    pub managed: bool,
    /// `null` when policy leaves it to Windows.
    pub automatic: Option<Automatic>,
    /// With WSUS: Windows never reaches Windows Update online.
    pub no_internet: bool,
    /// With WSUS: these come from Microsoft instead.
    pub quality_from_microsoft: bool,
    pub feature_from_microsoft: bool,
    /// Windows Update for Business deferrals, of updates from Microsoft.
    pub quality_deferral_days: u32,
    pub feature_deferral_days: u32,
}

/// `url`, when it fits: not empty, short, no control characters.
fn url(url: &str) -> Option<String> {
    (!url.is_empty() && url.len() <= MAX_URL && !url.chars().any(char::is_control))
        .then(|| url.to_owned())
}

impl UpdateSource {
    /// What the server keeps of a report's source; `None` for an origin
    /// it does not know, from an agent newer than itself.
    pub fn from_report(source: &WindowsUpdateSource) -> Option<Self> {
        let origin = match ProtoOrigin::try_from(source.origin).ok()? {
            ProtoOrigin::Microsoft => Origin::Microsoft,
            ProtoOrigin::Wsus => Origin::Wsus,
            ProtoOrigin::WsusWithoutUrl => Origin::WsusWithoutUrl,
            ProtoOrigin::Unspecified => return None,
        };
        let automatic = match ProtoAutomatic::try_from(source.automatic) {
            Ok(ProtoAutomatic::Off) => Some(Automatic::Off),
            Ok(ProtoAutomatic::Notify) => Some(Automatic::Notify),
            Ok(ProtoAutomatic::Download) => Some(Automatic::Download),
            Ok(ProtoAutomatic::Install) => Some(Automatic::Install),
            Ok(ProtoAutomatic::LocalChoice) => Some(Automatic::LocalChoice),
            Ok(ProtoAutomatic::Unspecified) | Err(_) => None,
        };
        let wsus_url = url(&source.wsus_url);
        // A WSUS origin without a usable URL is one without a URL.
        let origin = if origin == Origin::Wsus && wsus_url.is_none() {
            Origin::WsusWithoutUrl
        } else {
            origin
        };
        Some(Self {
            origin,
            wsus_url,
            status_url: url(&source.status_url),
            download_url: url(&source.download_url),
            managed: source.managed,
            automatic,
            no_internet: source.no_internet,
            quality_from_microsoft: source.quality_from_microsoft,
            feature_from_microsoft: source.feature_from_microsoft,
            quality_deferral_days: source.quality_deferral_days.min(MAX_DEFERRAL_DAYS),
            feature_deferral_days: source.feature_deferral_days.min(MAX_DEFERRAL_DAYS),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_what_fits() {
        let report = WindowsUpdateSource {
            origin: ProtoOrigin::Wsus.into(),
            wsus_url: "http://wsus.example:8530".into(),
            status_url: "x".repeat(MAX_URL + 1),
            download_url: "http://cache\0.example".into(),
            managed: true,
            automatic: ProtoAutomatic::Download.into(),
            quality_deferral_days: 9999,
            ..WindowsUpdateSource::default()
        };
        let source = UpdateSource::from_report(&report).unwrap();
        assert_eq!(source.origin, Origin::Wsus);
        assert_eq!(source.wsus_url.as_deref(), Some("http://wsus.example:8530"));
        assert_eq!((&source.status_url, &source.download_url), (&None, &None));
        assert_eq!(source.automatic, Some(Automatic::Download));
        assert_eq!(source.quality_deferral_days, MAX_DEFERRAL_DAYS);
        assert_eq!(
            serde_json::to_value(&source).unwrap()["origin"],
            serde_json::json!("wsus")
        );
    }

    #[test]
    fn unknown_values() {
        let unknown = WindowsUpdateSource {
            origin: 99,
            ..WindowsUpdateSource::default()
        };
        assert_eq!(UpdateSource::from_report(&unknown), None);
        assert_eq!(
            UpdateSource::from_report(&WindowsUpdateSource::default()),
            None
        );
        let source = UpdateSource::from_report(&WindowsUpdateSource {
            origin: ProtoOrigin::Wsus.into(),
            wsus_url: "http://wsus\n.example".into(),
            automatic: 42,
            ..WindowsUpdateSource::default()
        })
        .unwrap();
        assert_eq!(source.origin, Origin::WsusWithoutUrl);
        assert_eq!(source.automatic, None);
    }
}
