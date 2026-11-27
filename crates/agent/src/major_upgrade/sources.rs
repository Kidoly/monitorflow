//! Pointing apt's sources at the next release (ADR-0182), in both formats:
//! the one-line entries of `sources.list` and `*.list`, and the deb822
//! stanzas of `*.sources`. Pure text in, text out, so every case is a unit
//! test; [`plan`] reads the files under a root and the helper writes them.
//!
//! On Ubuntu, `do-release-upgrade` rewrites the sources itself: the same
//! reading finds its archive's entries and lists the third-party ones it
//! switches off, and nothing here writes. On the RHEL family, [`plan`]
//! lists the third-party repositories leapp leaves alone (`repos`).
//!
//! An entry is the release's when its suite is the codename, or the
//! codename and `-updates`, `-security`, `-backports` or
//! `-proposed-updates`, its components are Debian's, and its URI is a
//! Debian archive (a `debian.org` host, or a path ending in `debian` or
//! `debian-security`, as local mirrors and caches keep it) or a MonitorFlow
//! mirror (`monitorflow.sources`). Its suite moves to the new codename,
//! and `non-free-firmware` is added after `non-free`. A Debian entry of
//! another suite (`stable`, `sid`) is left alone. Anything else is
//! third-party and switched off.

use std::fs;
use std::path::{Path, PathBuf};

use monitorflow_common::major_upgrade::{Method, Upgrade};

const SOURCES_LIST: &str = "etc/apt/sources.list";
const PARTS: &str = "etc/apt/sources.list.d";
/// MonitorFlow's mirror file (ADR-0032, ADR-0074).
pub const MONITORFLOW_FILE: &str = "monitorflow.sources";
/// The suffixes of a release's suites.
const SUFFIXES: &[&str] = &[
    "",
    "-updates",
    "-security",
    "-backports",
    "-proposed-updates",
];
const DEBIAN_COMPONENTS: &[&str] = &["main", "contrib", "non-free", "non-free-firmware"];
const UBUNTU_COMPONENTS: &[&str] = &["main", "restricted", "universe", "multiverse"];

/// One source file and what the upgrade makes of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePlan {
    /// Absolute, as apt sees it.
    pub path: PathBuf,
    pub original: String,
    pub rewritten: String,
}

impl FilePlan {
    pub fn changed(&self) -> bool {
        self.original != self.rewritten
    }
}

/// What the upgrade does to a host's sources.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub files: Vec<FilePlan>,
    /// Entries of the release that move to the new codename.
    pub release_entries: usize,
    /// Third-party entries switched off, as `file: line`.
    pub third_party: Vec<String>,
    /// Why the upgrade cannot go on with these sources, if it cannot.
    pub refusal: Option<String>,
    /// Ubuntu and the RHEL family: the server takes its packages from the
    /// MonitorFlow mirrors, which their tools do not use. The campaign
    /// puts its own sources back first (ADR-0182); the helper refuses to
    /// start while it is so.
    pub mirrored: bool,
}

/// What one file's rewrite found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rewritten {
    pub text: String,
    pub release_entries: usize,
    /// The entries switched off, one line each.
    pub third_party: Vec<String>,
    /// MonitorFlow mirror entries pinned to a named snapshot.
    pub pinned: Vec<String>,
}

/// How an entry is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Release,
    /// A Debian entry of a suite that is not the release's.
    Other,
    ThirdParty,
}

struct Context<'a> {
    upgrade: &'a Upgrade,
    /// `monitorflow.sources`: the URIs name MonitorFlow mirrors.
    monitorflow: bool,
}

impl Context<'_> {
    /// The new suite for `suite`, when it is one of the release's.
    fn new_suite(&self, suite: &str) -> Option<String> {
        let rest = suite.strip_prefix(self.upgrade.from_codename)?;
        SUFFIXES
            .contains(&rest)
            .then(|| format!("{}{rest}", self.upgrade.to_codename))
    }

    fn ubuntu(&self) -> bool {
        self.upgrade.method == Method::ReleaseUpgrade
    }

    /// The distribution's archive: `debian.org` or `ubuntu.com` hosts, or
    /// a path ending in the archive's name, as local mirrors and caches
    /// keep it.
    fn archive_uri(&self, uri: &str) -> bool {
        if self.monitorflow {
            return true;
        }
        let Some((scheme, rest)) = uri.split_once("://") else {
            return false;
        };
        if !matches!(scheme, "http" | "https") {
            return false;
        }
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = host.rsplit('@').next().unwrap_or(host);
        let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
        let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
        if self.ubuntu() {
            // PPAs end in `ubuntu` too.
            let ppa = host == "launchpad.net"
                || host.ends_with(".launchpad.net")
                || host.ends_with(".launchpadcontent.net");
            return !ppa
                && (host == "ubuntu.com"
                    || host.ends_with(".ubuntu.com")
                    || matches!(last, "ubuntu" | "ubuntu-ports"));
        }
        host == "debian.org"
            || host.ends_with(".debian.org")
            || matches!(last, "debian" | "debian-security")
    }

    fn classify(&self, uris: &[&str], suites: &[&str], components: &[&str]) -> Class {
        let known = if self.ubuntu() {
            UBUNTU_COMPONENTS
        } else {
            DEBIAN_COMPONENTS
        };
        let debian = !uris.is_empty()
            && uris.iter().all(|u| self.archive_uri(u))
            && components.iter().all(|c| known.contains(c));
        if !debian {
            return Class::ThirdParty;
        }
        if suites.iter().any(|s| self.new_suite(s).is_some()) {
            Class::Release
        } else {
            Class::Other
        }
    }

    /// A MonitorFlow mirror URI moved to the new release's mirror:
    /// `…/apt/debian-12-updates/current/` to `…/apt/debian-13-updates/current/`.
    /// `Err` with the snapshot when pinned to a named one.
    fn mirror_uri(&self, uri: &str) -> Result<String, String> {
        let old = format!("{}-{}", self.upgrade.distribution, self.upgrade.from);
        let new = format!("{}-{}", self.upgrade.distribution, self.upgrade.to);
        let segments: Vec<&str> = uri.split('/').collect();
        let Some(at) = segments
            .iter()
            .position(|s| *s == old || s.strip_prefix(&old).is_some_and(|r| r.starts_with('-')))
        else {
            return Ok(uri.to_owned());
        };
        if at == 0 || segments[at - 1] != "apt" {
            return Ok(uri.to_owned());
        }
        let snapshot = segments.get(at + 1).copied().unwrap_or("");
        if !snapshot.is_empty() && snapshot != "current" {
            return Err(snapshot.to_owned());
        }
        let mut out: Vec<String> = segments.iter().map(|s| (*s).to_owned()).collect();
        out[at] = format!("{new}{}", &segments[at][old.len()..]);
        Ok(out.join("/"))
    }

    fn components(&self, components: &[&str]) -> Vec<String> {
        let mut out = Vec::with_capacity(components.len() + 1);
        for c in components {
            out.push((*c).to_owned());
            if *c == "non-free" && !self.ubuntu() && !components.contains(&"non-free-firmware") {
                out.push("non-free-firmware".into());
            }
        }
        out
    }

    fn disabled_note(&self) -> String {
        format!(
            "# Disabled by MonitorFlow for the upgrade to {}:",
            self.upgrade.to_label()
        )
    }
}

/// One-line format: `deb [options] uri suite components…`.
pub fn rewrite_classic(text: &str, upgrade: &Upgrade) -> Rewritten {
    let context = Context {
        upgrade,
        monitorflow: false,
    };
    let mut out = Rewritten::default();
    for line in text.split_inclusive('\n') {
        let (body, end) = match line.strip_suffix('\n') {
            Some(body) => (body, "\n"),
            None => (line, ""),
        };
        let Some(entry) = Classic::parse(body) else {
            out.text.push_str(line);
            continue;
        };
        match context.classify(&[entry.uri], &[entry.suite], &entry.components) {
            Class::Other => out.text.push_str(line),
            Class::ThirdParty => {
                out.third_party.push(body.trim().to_owned());
                out.text.push_str(&context.disabled_note());
                out.text.push('\n');
                out.text.push_str("# ");
                out.text.push_str(body);
                out.text.push_str(end);
            }
            Class::Release => {
                out.release_entries += 1;
                let suite = context
                    .new_suite(entry.suite)
                    .unwrap_or_else(|| entry.suite.to_owned());
                let mut parts = vec![entry.kind.to_owned()];
                if let Some(options) = entry.options {
                    parts.push(options.to_owned());
                }
                parts.push(entry.uri.to_owned());
                parts.push(suite);
                parts.extend(context.components(&entry.components));
                out.text.push_str(&parts.join(" "));
                if let Some(comment) = entry.comment {
                    out.text.push(' ');
                    out.text.push_str(comment);
                }
                out.text.push_str(end);
            }
        }
    }
    out
}

/// One enabled one-line entry.
struct Classic<'a> {
    kind: &'a str,
    options: Option<&'a str>,
    uri: &'a str,
    suite: &'a str,
    components: Vec<&'a str>,
    comment: Option<&'a str>,
}

impl<'a> Classic<'a> {
    fn parse(line: &'a str) -> Option<Self> {
        let (body, comment) = match line.find(" #") {
            Some(at) => (&line[..at], Some(line[at + 1..].trim())),
            None => (line, None),
        };
        let body = body.trim();
        let (kind, rest) = body.split_once(char::is_whitespace)?;
        if !matches!(kind, "deb" | "deb-src") {
            return None;
        }
        let rest = rest.trim_start();
        let (options, rest) = match rest.strip_prefix('[') {
            Some(inner) => {
                let close = inner.find(']')?;
                (Some(&rest[..close + 2]), inner[close + 1..].trim_start())
            }
            None => (None, rest),
        };
        let mut words = rest.split_whitespace();
        let uri = words.next()?;
        let suite = words.next()?;
        Some(Self {
            kind,
            options,
            uri,
            suite,
            components: words.collect(),
            comment,
        })
    }
}

/// deb822 format: stanzas of `Field: value` separated by blank lines.
pub fn rewrite_deb822(text: &str, upgrade: &Upgrade, monitorflow: bool) -> Rewritten {
    let context = Context {
        upgrade,
        monitorflow,
    };
    let mut out = Rewritten::default();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut start = 0;
    while start < lines.len() {
        let mut end = start;
        while end < lines.len() && !lines[end].trim().is_empty() {
            end += 1;
        }
        stanza(&lines[start..end], &context, &mut out);
        // The blank lines between stanzas, as they were.
        while end < lines.len() && lines[end].trim().is_empty() {
            out.text.push_str(lines[end]);
            end += 1;
        }
        start = end;
    }
    out
}

/// A field of a stanza: its lines (with continuations) and value.
struct Field {
    name: String,
    first: usize,
    last: usize,
    value: String,
}

fn fields(lines: &[&str]) -> Vec<Field> {
    let mut fields: Vec<Field> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let body = line.trim_end_matches('\n');
        if body.starts_with('#') {
            continue;
        }
        if body.starts_with([' ', '\t']) {
            if let Some(field) = fields.last_mut() {
                field.last = i;
                field.value.push(' ');
                field.value.push_str(body.trim());
            }
            continue;
        }
        if let Some((name, value)) = body.split_once(':') {
            fields.push(Field {
                name: name.trim().to_ascii_lowercase(),
                first: i,
                last: i,
                value: value.trim().to_owned(),
            });
        }
    }
    fields
}

fn stanza(lines: &[&str], context: &Context<'_>, out: &mut Rewritten) {
    let fields = fields(lines);
    let get = |name: &str| fields.iter().find(|f| f.name == name);
    let words = |name: &str| -> Vec<&str> {
        get(name)
            .map(|f| f.value.split_whitespace().collect())
            .unwrap_or_default()
    };
    let enabled = get("enabled").is_none_or(|f| !f.value.eq_ignore_ascii_case("no"));
    let (uris, suites, components) = (words("uris"), words("suites"), words("components"));
    if !enabled || uris.is_empty() || suites.is_empty() {
        lines.iter().for_each(|l| out.text.push_str(l));
        return;
    }
    match context.classify(&uris, &suites, &components) {
        Class::Other => lines.iter().for_each(|l| out.text.push_str(l)),
        Class::ThirdParty => {
            out.third_party
                .push(format!("{} {}", uris.join(" "), suites.join(" ")));
            out.text.push_str(&context.disabled_note());
            out.text.push('\n');
            let mut skip = None;
            if let Some(field) = get("enabled") {
                skip = Some((field.first, field.last));
            }
            for (i, line) in lines.iter().enumerate() {
                if skip.is_some_and(|(a, b)| (a..=b).contains(&i)) {
                    continue;
                }
                out.text.push_str(line);
            }
            if !out.text.ends_with('\n') {
                out.text.push('\n');
            }
            out.text.push_str("Enabled: no\n");
        }
        Class::Release => {
            out.release_entries += 1;
            let new_suites: Vec<String> = suites
                .iter()
                .map(|s| context.new_suite(s).unwrap_or_else(|| (*s).to_owned()))
                .collect();
            let mut new_uris = Vec::with_capacity(uris.len());
            for uri in &uris {
                if context.monitorflow {
                    match context.mirror_uri(uri) {
                        Ok(uri) => new_uris.push(uri),
                        Err(snapshot) => {
                            out.pinned.push(format!("{uri} ({snapshot})"));
                            new_uris.push((*uri).to_owned());
                        }
                    }
                } else {
                    new_uris.push((*uri).to_owned());
                }
            }
            let replacements = [
                ("uris", new_uris.join(" ")),
                ("suites", new_suites.join(" ")),
                ("components", context.components(&components).join(" ")),
            ];
            let mut i = 0;
            while i < lines.len() {
                let replaced = replacements.iter().find_map(|(name, value)| {
                    get(name).filter(|f| f.first == i).map(|f| (f.last, value))
                });
                match replaced {
                    Some((last, value)) => {
                        let label = lines[i].split(':').next().unwrap_or("").trim_end();
                        out.text.push_str(&format!("{label}: {value}\n"));
                        i = last + 1;
                    }
                    None => {
                        out.text.push_str(lines[i]);
                        i += 1;
                    }
                }
            }
            if !lines.last().is_some_and(|l| l.ends_with('\n')) && out.text.ends_with('\n') {
                out.text.pop();
            }
        }
    }
}

/// Reads every source file under `root` and works out the rewrite: apt's
/// for Debian and Ubuntu, the repositories for the RHEL family.
pub fn plan(root: &Path, upgrade: &Upgrade) -> Plan {
    match upgrade.method {
        Method::Apt => apt_plan(root, upgrade),
        Method::ReleaseUpgrade => {
            let mut plan = apt_plan(root, upgrade);
            let mirrored = plan.files.iter().any(|f| {
                f.path.file_name().and_then(|n| n.to_str()) == Some(MONITORFLOW_FILE) && f.changed()
            });
            plan.mirrored = mirrored;
            plan
        }
        Method::Leapp => super::repos::plan(root, upgrade),
    }
}

/// Why the helper does not start on a server still on the mirrors, on
/// Ubuntu and the RHEL family.
pub const MIRROR_REFUSAL: &str = "the server still takes its packages from the MonitorFlow mirrors, which {tool} does not use; a campaign puts its own sources back first, and Revert package sources on its page does it by hand";

/// The tool that would not use the mirrors, for [`MIRROR_REFUSAL`].
pub fn tool(method: Method) -> &'static str {
    match method {
        Method::Apt => "apt",
        Method::ReleaseUpgrade => "do-release-upgrade",
        Method::Leapp => "leapp",
    }
}

/// Why the helper must not go on with `plan`: its refusal, or the
/// mirrors where the tool does not use them.
pub fn blocker(plan: &Plan, method: Method) -> Option<String> {
    plan.refusal.clone().or_else(|| {
        (plan.mirrored && method != Method::Apt)
            .then(|| MIRROR_REFUSAL.replace("{tool}", tool(method)))
    })
}

fn apt_plan(root: &Path, upgrade: &Upgrade) -> Plan {
    let mut plan = Plan::default();
    let mut paths = vec![PathBuf::from("/").join(SOURCES_LIST)];
    match fs::read_dir(root.join(PARTS)) {
        Ok(dir) => {
            let mut names: Vec<String> = dir
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".list") || n.ends_with(".sources"))
                .collect();
            names.sort();
            paths.extend(names.iter().map(|n| PathBuf::from("/").join(PARTS).join(n)));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            plan.refusal = Some(format!("cannot read /{PARTS}: {e}"));
            return plan;
        }
    }
    for path in paths {
        let disk = root.join(path.strip_prefix("/").unwrap_or(&path));
        let meta = match fs::symlink_metadata(&disk) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                plan.refusal = Some(format!("cannot read {}: {e}", path.display()));
                return plan;
            }
        };
        if !meta.is_file() {
            plan.refusal = Some(format!(
                "{} is not a regular file; replace it with one first",
                path.display()
            ));
            return plan;
        }
        let original = match fs::read_to_string(&disk) {
            Ok(text) => text,
            Err(e) => {
                plan.refusal = Some(format!("cannot read {}: {e}", path.display()));
                return plan;
            }
        };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let rewritten = if name.ends_with(".sources") {
            rewrite_deb822(&original, upgrade, name == MONITORFLOW_FILE)
        } else {
            rewrite_classic(&original, upgrade)
        };
        if !rewritten.pinned.is_empty() {
            plan.refusal = Some(format!(
                "a campaign pinned {} to a mirror snapshot: {}; wait until it restores them",
                path.display(),
                rewritten.pinned.join(", ")
            ));
        }
        plan.release_entries += rewritten.release_entries;
        plan.third_party.extend(
            rewritten
                .third_party
                .iter()
                .map(|line| format!("{}: {line}", path.display())),
        );
        plan.files.push(FilePlan {
            path,
            original,
            rewritten: rewritten.text,
        });
    }
    if plan.refusal.is_none() && plan.release_entries == 0 {
        plan.refusal = Some(format!(
            "no enabled source of {} was found in /{SOURCES_LIST} or /{PARTS}",
            upgrade.from_label()
        ));
    }
    plan
}
