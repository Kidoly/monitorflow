//! Where Windows Update takes updates from (ADR-0086): the WSUS server or
//! Microsoft, read from the Windows Update policy in the registry and
//! interpreted the way Windows does. [`interpret`] builds everywhere, so
//! Linux tests cover the contradictory cases; [`read`] is Windows only.

use monitorflow_common::proto::v1::windows_update_source::{Automatic, Origin};
use monitorflow_common::proto::v1::WindowsUpdateSource;

/// Group Policy's Windows Update key.
pub const POLICY_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate";
/// Group Policy's Automatic Updates key.
pub const AU_KEY: &str = r"SOFTWARE\Policies\Microsoft\Windows\WindowsUpdate\AU";
/// Where MDM (Intune) policies of the Update area land.
pub const MDM_KEY: &str = r"SOFTWARE\Microsoft\PolicyManager\current\device\Update";

/// Longest URL reported.
const MAX_URL: usize = 512;
/// Longest deferral Windows accepts: 365 days for feature updates, 30 for
/// quality updates.
const MAX_DEFERRAL_DAYS: u32 = 365;

/// The Windows Update values of Group Policy, as read; `None` is a value
/// that is not set. Fields are named after the registry values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    pub wu_server: Option<String>,
    pub wu_status_server: Option<String>,
    pub update_service_url_alternate: Option<String>,
    pub use_wu_server: Option<u32>,
    pub no_auto_update: Option<u32>,
    pub au_options: Option<u32>,
    pub do_not_connect_to_internet: Option<u32>,
    /// `SetPolicyDrivenUpdateSourceForQualityUpdates`: 0 Microsoft, 1 WSUS.
    pub quality_source: Option<u32>,
    /// `SetPolicyDrivenUpdateSourceForFeatureUpdates`.
    pub feature_source: Option<u32>,
    pub defer_quality_updates: Option<u32>,
    pub defer_quality_days: Option<u32>,
    pub defer_feature_updates: Option<u32>,
    pub defer_feature_days: Option<u32>,
}

impl Policy {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Fills the values Group Policy leaves unset from the MDM ones, as
    /// Windows gives Group Policy the last word only where both are set.
    pub fn or_mdm(mut self, mdm: &Mdm) -> Self {
        let url = mdm
            .update_service_url
            .as_deref()
            .map(str::trim)
            .filter(|u| is_http(u));
        if self.wu_server.is_none() && self.use_wu_server.is_none() {
            if let Some(url) = url {
                self.wu_server = Some(url.to_owned());
                self.use_wu_server = Some(1);
            }
        }
        if self.update_service_url_alternate.is_none() {
            self.update_service_url_alternate = mdm
                .update_service_url_alternate
                .clone()
                .filter(|u| is_http(u.trim()));
        }
        if self.no_auto_update.is_none() && self.au_options.is_none() {
            // AllowAutoUpdate: 0 notify before downloading, 1 to 4 install
            // on their own with various restart rules, 5 off.
            match mdm.allow_auto_update {
                Some(0) => self.au_options = Some(2),
                Some(1..=4) => self.au_options = Some(4),
                Some(5) => self.no_auto_update = Some(1),
                _ => {}
            }
        }
        let fill = |gpo: &mut Option<u32>, value: Option<u32>| {
            if gpo.is_none() {
                *gpo = value;
            }
        };
        fill(
            &mut self.do_not_connect_to_internet,
            mdm.do_not_connect_to_internet,
        );
        fill(&mut self.quality_source, mdm.quality_source);
        fill(&mut self.feature_source, mdm.feature_source);
        // MDM has no on/off switch next to the days.
        if self.defer_quality_updates.is_none() && mdm.defer_quality_days.is_some() {
            self.defer_quality_updates = Some(1);
            self.defer_quality_days = mdm.defer_quality_days;
        }
        if self.defer_feature_updates.is_none() && mdm.defer_feature_days.is_some() {
            self.defer_feature_updates = Some(1);
            self.defer_feature_days = mdm.defer_feature_days;
        }
        self
    }
}

/// The same settings set by MDM, under [`MDM_KEY`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mdm {
    pub update_service_url: Option<String>,
    pub update_service_url_alternate: Option<String>,
    pub allow_auto_update: Option<u32>,
    pub do_not_connect_to_internet: Option<u32>,
    pub quality_source: Option<u32>,
    pub feature_source: Option<u32>,
    pub defer_quality_days: Option<u32>,
    pub defer_feature_days: Option<u32>,
}

fn is_http(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// `url` trimmed, without control characters or user information, cut
/// to [`MAX_URL`]; `None` when nothing is left. A WSUS URL holds no
/// secret, but one written as `http://user:password@host` would.
pub fn clean_url(url: &str) -> Option<String> {
    let url: String = url.trim().chars().filter(|c| !c.is_control()).collect();
    let url = match url.split_once("://") {
        Some((scheme, rest)) => {
            let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let (authority, path) = rest.split_at(authority_end);
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            format!("{scheme}://{host}{path}")
        }
        None => url,
    };
    let url: String = url.chars().take(MAX_URL).collect();
    (!url.is_empty()).then_some(url)
}

/// The same server: case and a trailing slash aside.
fn same_url(a: &str, b: &str) -> bool {
    a.trim_end_matches('/')
        .eq_ignore_ascii_case(b.trim_end_matches('/'))
}

fn deferral(enabled: Option<u32>, days: Option<u32>) -> u32 {
    if enabled == Some(1) {
        days.unwrap_or(0).min(MAX_DEFERRAL_DAYS)
    } else {
        0
    }
}

/// What Windows makes of `policy`:
///
/// - WSUS is used when `UseWUServer` is 1. A `WUServer` without it is
///   ignored by Windows, and reported as such; `UseWUServer` without a
///   `WUServer` is WSUS without a server.
/// - `NoAutoUpdate` 1 turns automatic updates off whatever `AUOptions`
///   says; an `AUOptions` Windows does not know is reported as unset.
/// - The status and download servers, the ban on Internet locations and
///   the policy-driven source matter only with WSUS.
/// - Deferrals matter only for updates that come from Microsoft: WSUS
///   approvals decide the rest.
pub fn interpret(policy: &Policy) -> WindowsUpdateSource {
    let wsus_url = policy.wu_server.as_deref().and_then(clean_url);
    let origin = match (policy.use_wu_server == Some(1), &wsus_url) {
        (true, Some(_)) => Origin::Wsus,
        (true, None) => Origin::WsusWithoutUrl,
        (false, _) => Origin::Microsoft,
    };
    let wsus = origin != Origin::Microsoft;
    let status_url = policy
        .wu_status_server
        .as_deref()
        .and_then(clean_url)
        .filter(|status| wsus && !wsus_url.as_deref().is_some_and(|w| same_url(w, status)));
    let download_url = policy
        .update_service_url_alternate
        .as_deref()
        .and_then(clean_url)
        .filter(|_| wsus);
    let automatic = if policy.no_auto_update == Some(1) {
        Automatic::Off
    } else {
        match policy.au_options {
            Some(2) => Automatic::Notify,
            Some(3 | 7) => Automatic::Download,
            Some(4) => Automatic::Install,
            Some(5) => Automatic::LocalChoice,
            _ => Automatic::Unspecified,
        }
    };
    let quality_from_microsoft = wsus && policy.quality_source == Some(0);
    let feature_from_microsoft = wsus && policy.feature_source == Some(0);
    let quality_deferral_days = if !wsus || quality_from_microsoft {
        deferral(policy.defer_quality_updates, policy.defer_quality_days)
    } else {
        0
    };
    let feature_deferral_days = if !wsus || feature_from_microsoft {
        deferral(policy.defer_feature_updates, policy.defer_feature_days)
    } else {
        0
    };
    WindowsUpdateSource {
        origin: origin.into(),
        wsus_url: wsus_url.unwrap_or_default(),
        status_url: status_url.unwrap_or_default(),
        download_url: download_url.unwrap_or_default(),
        managed: !policy.is_empty(),
        automatic: automatic.into(),
        no_internet: wsus && policy.do_not_connect_to_internet == Some(1),
        quality_from_microsoft,
        feature_from_microsoft,
        quality_deferral_days,
        feature_deferral_days,
    }
}

/// Reads the policy of this host. Keys that do not exist are policies
/// that are not set; values of the wrong type are ignored, as Windows
/// ignores them.
#[cfg(windows)]
pub fn read() -> Result<WindowsUpdateSource, String> {
    let wu = open(POLICY_KEY)?;
    let au = open(AU_KEY)?;
    let mdm = open(MDM_KEY)?;
    let string = |key: &Option<windows_registry::Key>, name: &str| {
        key.as_ref().and_then(|k| k.get_string(name).ok())
    };
    let number = |key: &Option<windows_registry::Key>, name: &str| {
        key.as_ref().and_then(|k| k.get_u32(name).ok())
    };
    let policy = Policy {
        wu_server: string(&wu, "WUServer"),
        wu_status_server: string(&wu, "WUStatusServer"),
        update_service_url_alternate: string(&wu, "UpdateServiceUrlAlternate"),
        use_wu_server: number(&au, "UseWUServer"),
        no_auto_update: number(&au, "NoAutoUpdate"),
        au_options: number(&au, "AUOptions"),
        do_not_connect_to_internet: number(&wu, "DoNotConnectToWindowsUpdateInternetLocations"),
        quality_source: number(&wu, "SetPolicyDrivenUpdateSourceForQualityUpdates"),
        feature_source: number(&wu, "SetPolicyDrivenUpdateSourceForFeatureUpdates"),
        defer_quality_updates: number(&wu, "DeferQualityUpdates"),
        defer_quality_days: number(&wu, "DeferQualityUpdatesPeriodInDays"),
        defer_feature_updates: number(&wu, "DeferFeatureUpdates"),
        defer_feature_days: number(&wu, "DeferFeatureUpdatesPeriodInDays"),
    };
    let mdm = Mdm {
        update_service_url: string(&mdm, "UpdateServiceUrl"),
        update_service_url_alternate: string(&mdm, "UpdateServiceUrlAlternate"),
        allow_auto_update: number(&mdm, "AllowAutoUpdate"),
        do_not_connect_to_internet: number(&mdm, "DoNotConnectToWindowsUpdateInternetLocations"),
        quality_source: number(&mdm, "SetPolicyDrivenUpdateSourceForQualityUpdates"),
        feature_source: number(&mdm, "SetPolicyDrivenUpdateSourceForFeatureUpdates"),
        defer_quality_days: number(&mdm, "DeferQualityUpdatesPeriodInDays"),
        defer_feature_days: number(&mdm, "DeferFeatureUpdatesPeriodInDays"),
    };
    Ok(interpret(&policy.or_mdm(&mdm)))
}

/// `HKLM\path`, or `None` when it does not exist.
#[cfg(windows)]
fn open(path: &str) -> Result<Option<windows_registry::Key>, String> {
    /// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
    const NOT_FOUND: u32 = 0x8007_0002;
    match windows_registry::LOCAL_MACHINE.open(path) {
        Ok(key) => Ok(Some(key)),
        Err(e) if e.code().0 as u32 == NOT_FOUND => Ok(None),
        Err(e) => Err(format!(r"HKLM\{path}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wsus(url: &str) -> Policy {
        Policy {
            wu_server: Some(url.into()),
            wu_status_server: Some(url.into()),
            use_wu_server: Some(1),
            ..Policy::default()
        }
    }

    #[test]
    fn no_policy_is_microsoft_unmanaged() {
        let source = interpret(&Policy::default());
        assert_eq!(source.origin(), Origin::Microsoft);
        assert!(!source.managed);
        assert_eq!(source.automatic(), Automatic::Unspecified);
        assert_eq!(source.wsus_url, "");
    }

    #[test]
    fn wsus_with_its_status_server() {
        let source = interpret(&wsus("http://wsus.example:8530"));
        assert_eq!(source.origin(), Origin::Wsus);
        assert!(source.managed);
        assert_eq!(source.wsus_url, "http://wsus.example:8530");
        // The same server, written another way, is not repeated.
        let mut policy = wsus("http://wsus.example:8530");
        policy.wu_status_server = Some("HTTP://WSUS.example:8530/".into());
        assert_eq!(interpret(&policy).status_url, "");
        policy.wu_status_server = Some("http://reports.example:8530".into());
        policy.update_service_url_alternate = Some("http://cache.example".into());
        let source = interpret(&policy);
        assert_eq!(source.status_url, "http://reports.example:8530");
        assert_eq!(source.download_url, "http://cache.example");
    }

    #[test]
    fn a_wsus_url_without_use_wu_server_is_ignored() {
        for use_wu_server in [None, Some(0), Some(2)] {
            let policy = Policy {
                use_wu_server,
                update_service_url_alternate: Some("http://cache.example".into()),
                wu_status_server: Some("http://reports.example".into()),
                do_not_connect_to_internet: Some(1),
                quality_source: Some(0),
                ..wsus("http://wsus.example:8530")
            };
            let source = interpret(&policy);
            assert_eq!(source.origin(), Origin::Microsoft, "{use_wu_server:?}");
            // Reported, so the page can say it is not used.
            assert_eq!(source.wsus_url, "http://wsus.example:8530");
            assert_eq!(source.status_url, "");
            assert_eq!(source.download_url, "");
            assert!(!source.no_internet);
            assert!(!source.quality_from_microsoft);
        }
    }

    #[test]
    fn use_wu_server_without_a_url() {
        for url in [None, Some(""), Some("  \r\n")] {
            let policy = Policy {
                wu_server: url.map(Into::into),
                use_wu_server: Some(1),
                ..Policy::default()
            };
            let source = interpret(&policy);
            assert_eq!(source.origin(), Origin::WsusWithoutUrl, "{url:?}");
            assert_eq!(source.wsus_url, "");
        }
    }

    #[test]
    fn automatic_update_modes() {
        let mode = |no_auto_update, au_options| {
            interpret(&Policy {
                no_auto_update,
                au_options,
                ..Policy::default()
            })
            .automatic()
        };
        assert_eq!(mode(None, Some(2)), Automatic::Notify);
        assert_eq!(mode(Some(0), Some(3)), Automatic::Download);
        assert_eq!(mode(None, Some(7)), Automatic::Download);
        assert_eq!(mode(None, Some(4)), Automatic::Install);
        assert_eq!(mode(None, Some(5)), Automatic::LocalChoice);
        // Off wins over any mode.
        assert_eq!(mode(Some(1), Some(4)), Automatic::Off);
        assert_eq!(mode(Some(1), None), Automatic::Off);
        // Values Windows does not know.
        assert_eq!(mode(None, Some(1)), Automatic::Unspecified);
        assert_eq!(mode(None, Some(6)), Automatic::Unspecified);
        assert_eq!(mode(Some(0), None), Automatic::Unspecified);
    }

    #[test]
    fn wsus_settings_that_send_updates_to_microsoft() {
        let policy = Policy {
            do_not_connect_to_internet: Some(1),
            quality_source: Some(0),
            feature_source: Some(1),
            defer_quality_updates: Some(1),
            defer_quality_days: Some(7),
            defer_feature_updates: Some(1),
            defer_feature_days: Some(180),
            ..wsus("http://wsus.example:8530")
        };
        let source = interpret(&policy);
        assert!(source.no_internet);
        assert!(source.quality_from_microsoft);
        assert!(!source.feature_from_microsoft);
        // WSUS approvals decide feature updates: their deferral is moot.
        assert_eq!(source.quality_deferral_days, 7);
        assert_eq!(source.feature_deferral_days, 0);
    }

    #[test]
    fn deferrals_from_microsoft() {
        let policy = Policy {
            defer_quality_updates: Some(1),
            defer_quality_days: Some(14),
            // Days without the switch, and too many days.
            defer_feature_updates: Some(0),
            defer_feature_days: Some(90),
            ..Policy::default()
        };
        let source = interpret(&policy);
        assert_eq!(source.origin(), Origin::Microsoft);
        assert_eq!(source.quality_deferral_days, 14);
        assert_eq!(source.feature_deferral_days, 0);
        let policy = Policy {
            defer_feature_updates: Some(1),
            defer_feature_days: Some(5000),
            ..Policy::default()
        };
        assert_eq!(interpret(&policy).feature_deferral_days, 365);
    }

    #[test]
    fn urls_lose_user_information_and_control_characters() {
        assert_eq!(
            clean_url(" http://user:secret@wsus.example:8530/path?x=1 ").as_deref(),
            Some("http://wsus.example:8530/path?x=1")
        );
        assert_eq!(
            clean_url("https://wsus\n.example").as_deref(),
            Some("https://wsus.example")
        );
        assert_eq!(
            clean_url("wsus.example:8530").as_deref(),
            Some("wsus.example:8530")
        );
        assert_eq!(clean_url(&"h".repeat(2000)).map(|u| u.len()), Some(MAX_URL));
        assert_eq!(clean_url(" "), None);
    }

    #[test]
    fn mdm_fills_what_group_policy_leaves_unset() {
        let mdm = Mdm {
            update_service_url: Some("https://wsus.intune.example:8531".into()),
            allow_auto_update: Some(0),
            do_not_connect_to_internet: Some(1),
            defer_quality_days: Some(3),
            ..Mdm::default()
        };
        let source = interpret(&Policy::default().or_mdm(&mdm));
        assert_eq!(source.origin(), Origin::Wsus);
        assert_eq!(source.wsus_url, "https://wsus.intune.example:8531");
        assert_eq!(source.automatic(), Automatic::Notify);
        assert!(source.no_internet);
        // From WSUS: the deferral does not apply.
        assert_eq!(source.quality_deferral_days, 0);

        // Group Policy keeps the last word where it speaks.
        let gpo = Policy {
            use_wu_server: Some(0),
            au_options: Some(3),
            ..Policy::default()
        };
        let source = interpret(&gpo.or_mdm(&mdm));
        assert_eq!(source.origin(), Origin::Microsoft);
        assert_eq!(source.automatic(), Automatic::Download);
        assert_eq!(source.quality_deferral_days, 3);

        // MDM's placeholder is not a URL, and 5 is off.
        let mdm = Mdm {
            update_service_url: Some("CorpWSUS".into()),
            allow_auto_update: Some(5),
            ..Mdm::default()
        };
        let source = interpret(&Policy::default().or_mdm(&mdm));
        assert_eq!(source.origin(), Origin::Microsoft);
        assert_eq!(source.automatic(), Automatic::Off);
        assert!(source.managed);
    }

    /// Reads this host's real policy, read-only: the runner has none or
    /// some, either way the read must work as the test's user.
    #[cfg(windows)]
    #[test]
    fn reads_this_hosts_policy() {
        let source = read().expect("the Windows Update policy is readable");
        assert_ne!(source.origin(), Origin::Unspecified);
    }
}
