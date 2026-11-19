//! On-demand syncs (ADR-0142): the indexes of a mirror and nothing else,
//! downloaded by the server itself rather than by `aptly` or `reposync`.
//!
//! A sync builds a new working tree in `ondemand-work/.new-<mirror>`, then
//! puts it in place of `ondemand-work/<mirror>`. Every index file comes
//! from the content store ([`Store`]), downloaded only when the store does
//! not have its checksum, and is hardlinked into the tree; a snapshot then
//! hardlinks the tree again (`layout::link_tree`), so an index shared by
//! snapshots or mirrors is on the disk once.
//!
//! - apt: the upstream's `Release` and `Release.gpg` go to `.upstream/`
//!   for the job to verify with gpg; then one compression of each
//!   `Packages` index, the `Release` of its directory and the English
//!   translations, each checked against `Release`; then a `Release` of our
//!   own that lists only those, which the snapshot signs.
//! - dnf: `repodata/repomd.xml`, kept as it is and signed by the snapshot,
//!   and every file it lists, checked against it.
//!
//! Then the package list (`.packages`) of every package those indexes
//! name, and `.origin`, which says where to fetch them from.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use monitorflow_common::mirror::origin::{Origin, ORIGIN};
use monitorflow_common::mirror::packages::{ListError, PackageList, PACKAGES};
use monitorflow_common::mirror::repo::compress::{self, Compression};
use monitorflow_common::mirror::repo::fetch::{Expect, FetchError, Fetcher};
use monitorflow_common::mirror::repo::meta::{Algorithm, Entry, MetaError, Package};
use monitorflow_common::mirror::repo::{apt, dnf};
use monitorflow_common::mirror::store::{self as content, Store};

use super::layout::{self, Layout};
use super::model::{Family, Mirror};

/// Bounds connecting to the upstream and every wait for more data, as
/// relays do (ADR-0034).
pub const TIMEOUT: Duration = Duration::from_secs(600);

/// Largest `Release`, `Release.gpg` or `repomd.xml`, whose size nothing
/// gives in advance (ADR-0083).
const ROOT_LIMIT: u64 = 16 * 1024 * 1024;

/// Directory of the upstream's own signed files in a staging tree, not
/// linked into snapshots.
const UPSTREAM: &str = ".upstream";

/// Fields of the upstream's `Release` that ours keeps, as they are.
const KEPT_FIELDS: [&str; 8] = [
    "Origin",
    "Label",
    "Suite",
    "Version",
    "Codename",
    "Changelogs",
    "Description",
    "No-Support-for-Architecture-all",
];

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Fetch(#[from] FetchError),
    #[error(transparent)]
    Meta(#[from] MetaError),
    #[error(transparent)]
    List(#[from] ListError),
    #[error("{0} is missing upstream")]
    Missing(String),
    #[error("the upstream's Release lists no {0}")]
    NoIndex(String),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot reach the upstream: {0}")]
    Client(String),
    #[error("a sync task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> SyncError + '_ {
    move |source| SyncError::Io {
        path: path.to_owned(),
        source,
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SyncError> + Send + 'static,
) -> Result<T, SyncError> {
    tokio::task::spawn_blocking(f).await?
}

/// The HTTP client of on-demand fetches toward upstreams, at no base URL
/// of its own: each `.origin` gives one.
pub fn client() -> Fetcher {
    Fetcher::with_agent("http://upstream.invalid", TIMEOUT, "monitorflow-server")
        // Only fails when the TLS backend cannot start, which every other
        // client of the server would hit first.
        .expect("the HTTP client of on-demand mirrors")
}

/// The minor release `.treeinfo` names in its `[general]` section, like
/// `version = 9.8`; `None` when it gives the major release only.
pub fn treeinfo_minor(text: &str) -> Option<u32> {
    let mut general = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            general = line == "[general]";
        } else if general {
            if let Some(("version", value)) =
                line.split_once('=').map(|(k, v)| (k.trim(), v.trim()))
            {
                return value.split_once('.')?.1.parse().ok();
            }
        }
    }
    None
}

/// What a sync did, for the run's log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    /// Index files downloaded, and their bytes.
    pub downloaded: u64,
    pub bytes: u64,
    /// Index files the store had already.
    pub reused: u64,
    /// Packages the indexes list.
    pub packages: usize,
}

/// A sync in progress.
pub struct Staging {
    mirror: Mirror,
    fetcher: Fetcher,
    store: Store,
    /// The new working tree.
    dir: PathBuf,
    /// Where it goes once complete.
    work: PathBuf,
    now: DateTime<Utc>,
    /// dnf: the minor release the upstream's tree says it is.
    minor: Option<u32>,
    stats: Stats,
}

impl Staging {
    /// Starts a sync of `mirror` from an empty staging tree.
    pub async fn start(layout: &Layout, mirror: &Mirror) -> Result<Self, SyncError> {
        let fetcher = Fetcher::with_agent(&mirror.upstream_url, TIMEOUT, "monitorflow-server")
            .map_err(|e| SyncError::Client(e.to_string()))?;
        let store = layout.store();
        let dir = layout.ondemand_staging(mirror);
        let (s, d) = (store.clone(), dir.clone());
        blocking(move || {
            s.create().map_err(io(s.root()))?;
            layout::remove_tree(&d).map_err(io(&d))?;
            std::fs::create_dir_all(d.join(UPSTREAM)).map_err(io(&d))
        })
        .await?;
        Ok(Self {
            mirror: mirror.clone(),
            fetcher,
            store,
            dir,
            work: layout.ondemand_work(mirror),
            now: Utc::now(),
            minor: None,
            stats: Stats::default(),
        })
    }

    /// apt: downloads the suite's `Release` and `Release.gpg`, which the
    /// caller must verify before [`Self::apt_indexes`]. Returns their
    /// paths.
    pub async fn apt_roots(&mut self) -> Result<(PathBuf, PathBuf), SyncError> {
        let dists = apt::suite_dir(&self.mirror.distribution);
        let mut paths = Vec::new();
        for name in ["Release", "Release.gpg"] {
            let dest = self.dir.join(UPSTREAM).join(name);
            let path = format!("{dists}/{name}");
            self.fetcher
                .file(&path, &dest, Expect::AtMost(ROOT_LIMIT))
                .await?
                .ok_or(SyncError::Missing(path))?;
            paths.push(dest);
        }
        let signature = paths.pop().expect("two paths");
        let release = paths.pop().expect("two paths");
        Ok((release, signature))
    }

    /// apt, once the upstream's `Release` is verified: the indexes, our
    /// `Release` and the package list.
    pub async fn apt_indexes(&mut self) -> Result<(), SyncError> {
        let upstream = self.dir.join(UPSTREAM).join("Release");
        let text = tokio::fs::read_to_string(&upstream)
            .await
            .map_err(io(&upstream))?;
        let suite = self.mirror.distribution.clone();
        upstream_release_fits(&text, &suite, self.now)?;
        let listed = apt::release(&suite, &text)?;
        let architectures = if self.mirror.architectures.is_empty() {
            field(&text, "Architectures")
                .unwrap_or_default()
                .split_whitespace()
                .filter(|a| *a != "all")
                .map(str::to_owned)
                .collect()
        } else {
            self.mirror.architectures.clone()
        };
        let dists = apt::suite_dir(&suite);
        let mut kept = Vec::new();
        let mut packages = Vec::new();
        for component in self.mirror.components.clone() {
            for arch in &architectures {
                let dir = format!("{dists}/{component}/binary-{arch}");
                let (entry, stored) = self
                    .first_of(&listed, &dir, "Packages", &[".xz", ".gz", ""])
                    .await?
                    .ok_or_else(|| {
                        SyncError::NoIndex(format!("{component}/binary-{arch}/Packages"))
                    })?;
                kept.extend(self.plain_entry(&entry, &stored).await?);
                let file = entry.path.clone();
                packages
                    .extend(blocking(move || Ok(apt::read_package_list(&file, &stored)?)).await?);
                kept.push(entry);
                if let Some((entry, _)) = self.first_of(&listed, &dir, "Release", &[""]).await? {
                    kept.push(entry);
                }
            }
            let i18n = format!("{dists}/{component}/i18n");
            let translations = [".xz", ".bz2", ".gz", ""];
            if let Some((entry, stored)) = self
                .first_of(&listed, &i18n, "Translation-en", &translations)
                .await?
            {
                kept.extend(self.plain_entry(&entry, &stored).await?);
                kept.push(entry);
            }
        }
        let release = release_file(
            &text,
            &suite,
            &self.mirror.components,
            &architectures,
            &kept,
            self.now,
        );
        let path = self.dir.join(&dists).join("Release");
        tokio::fs::write(&path, release).await.map_err(io(&path))?;
        self.package_list(packages).await
    }

    /// dnf: `repomd.xml`, every file it lists and the package list.
    pub async fn dnf(&mut self) -> Result<(), SyncError> {
        let repomd = self.dir.join(dnf::REPOMD);
        self.fetcher
            .file(dnf::REPOMD, &repomd, Expect::AtMost(ROOT_LIMIT))
            .await?
            .ok_or_else(|| SyncError::Missing(dnf::REPOMD.into()))?;
        let metadata = blocking(move || {
            let file = std::fs::File::open(&repomd).map_err(io(&repomd))?;
            Ok(dnf::repomd(std::io::BufReader::new(file))?)
        })
        .await?;
        let mut packages = None;
        for data in metadata {
            let stored = self
                .obtain(&data.entry)
                .await?
                .ok_or_else(|| SyncError::Missing(data.entry.path.clone()))?;
            if data.kind == "primary" {
                let file = data.entry.path.clone();
                packages =
                    Some(blocking(move || Ok(dnf::primary_packages(&file, &stored)?)).await?);
            }
        }
        let packages = packages.ok_or_else(|| SyncError::Missing("primary metadata".into()))?;
        // Which vault holds this state once the next minor release is out;
        // optional, as most repositories have no `.treeinfo`.
        let treeinfo = self.dir.join(UPSTREAM).join("treeinfo");
        if let Ok(Some(_)) = self
            .fetcher
            .file(".treeinfo", &treeinfo, Expect::AtMost(1024 * 1024))
            .await
        {
            let text = tokio::fs::read_to_string(&treeinfo)
                .await
                .unwrap_or_default();
            self.minor = treeinfo_minor(&text);
        }
        self.package_list(packages).await
    }

    /// Writes `.origin` and puts the tree in place of the working tree.
    pub async fn finish(self) -> Result<Stats, SyncError> {
        let origin = Origin {
            upstream: self.mirror.upstream_url.clone(),
            archives: super::catalog::archives(&self.mirror.name, self.now, self.minor),
        };
        let (dir, work) = (self.dir, self.work);
        blocking(move || {
            let path = dir.join(ORIGIN);
            origin.write(&path).map_err(io(&path))?;
            let old = work.with_file_name(format!(
                ".old-{}",
                work.file_name().unwrap_or_default().to_string_lossy()
            ));
            layout::remove_tree(&old).map_err(io(&old))?;
            if work.exists() {
                std::fs::rename(&work, &old).map_err(io(&work))?;
            }
            std::fs::rename(&dir, &work).map_err(io(&dir))?;
            layout::remove_tree(&old).map_err(io(&old))
        })
        .await?;
        Ok(self.stats)
    }

    /// The first of `<dir>/<name><extension>` that `listed` names and the
    /// upstream serves, in the order of `extensions`, with its stored path,
    /// linked into the tree. Publishers list compressions they do not
    /// generate.
    async fn first_of(
        &mut self,
        listed: &[Entry],
        dir: &str,
        name: &str,
        extensions: &[&str],
    ) -> Result<Option<(Entry, PathBuf)>, SyncError> {
        for extension in extensions {
            let path = format!("{dir}/{name}{extension}");
            let Some(entry) = listed.iter().find(|e| e.path == path) else {
                continue;
            };
            if let Some(stored) = self.obtain(entry).await? {
                return Ok(Some((entry.clone(), stored)));
            }
        }
        Ok(None)
    }

    /// The entry of the decompressed form of the compressed index `entry`,
    /// stored at `stored`; `None` when it is not compressed. apt checks an
    /// index by its decompressed form and skips one whose form the Release
    /// does not list, so ours lists it, as Debian's do, though only the
    /// compressed file is served.
    async fn plain_entry(&self, entry: &Entry, stored: &Path) -> Result<Option<Entry>, SyncError> {
        let (file, stored) = (entry.path.clone(), stored.to_owned());
        blocking(move || Ok(plain_entry(&file, &stored)?)).await
    }

    /// Puts the index file `entry` in the store unless it is there,
    /// checked against its size and checksum, links it into the tree and
    /// returns its stored path; `None` when the upstream does not have it.
    async fn obtain(&mut self, entry: &Entry) -> Result<Option<PathBuf>, SyncError> {
        let known = (entry.checksum.algorithm == Algorithm::Sha256)
            .then(|| self.store.index_path(&entry.checksum))
            .filter(|p| p.is_file());
        let stored = match known {
            Some(stored) => {
                self.stats.reused += 1;
                stored
            }
            None => {
                let temp = self.store.temp_path();
                let Some(got) = self
                    .fetcher
                    .file(&entry.path, &temp, Expect::Entry(entry))
                    .await?
                else {
                    return Ok(None);
                };
                self.stats.downloaded += 1;
                self.stats.bytes += got.size;
                let store = self.store.clone();
                blocking(move || {
                    store
                        .keep_index(&temp, &got.checksum)
                        .map_err(io(store.root()))
                })
                .await?
            }
        };
        let (from, to) = (stored.clone(), self.dir.join(&entry.path));
        blocking(move || content::link(&from, &to).map_err(io(&to))).await?;
        Ok(Some(stored))
    }

    /// Writes the package list into the store and links it as `.packages`.
    async fn package_list(&mut self, packages: Vec<Package>) -> Result<(), SyncError> {
        let list = PackageList::new(packages)?;
        self.stats.packages = list.len();
        let (store, dest) = (self.store.clone(), self.dir.join(PACKAGES));
        blocking(move || {
            let text = list.to_text();
            let mut hasher = Algorithm::Sha256.hasher();
            hasher.update(text.as_bytes());
            let temp = store.temp_path();
            std::fs::write(&temp, text).map_err(io(&temp))?;
            let stored = store
                .keep_index(&temp, &hasher.finish())
                .map_err(io(store.root()))?;
            content::link(&stored, &dest).map_err(io(&dest))
        })
        .await
    }
}

/// See [`Staging::plain_entry`]. Blocking.
fn plain_entry(file: &str, stored: &Path) -> Result<Option<Entry>, MetaError> {
    let (compression, plain) = Compression::of(file);
    if compression == Compression::Plain {
        return Ok(None);
    }
    let mut reader = compress::open(file, stored)?;
    let mut hasher = Algorithm::Sha256.hasher();
    let mut size = 0;
    let mut buf = vec![0; 64 * 1024];
    loop {
        let read = std::io::Read::read(&mut reader, &mut buf)
            .map_err(|e| MetaError::invalid(file, e.to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        size += read as u64;
    }
    Ok(Some(Entry {
        path: plain.to_owned(),
        size,
        checksum: hasher.finish(),
    }))
}

/// The value of the single-line field `name` of a deb822 paragraph.
fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key == name).then(|| value.trim())
    })
}

/// Refuses an upstream `Release` that is signed but not the one asked
/// for: another suite of the same archive (`-proposed` for `-updates`), or
/// one past its `Valid-Until`, which an old copy replayed would be. Ours
/// drops that field, so the check cannot be left to apt on the hosts.
fn upstream_release_fits(text: &str, suite: &str, now: DateTime<Utc>) -> Result<(), MetaError> {
    let file = format!("{}/Release", apt::suite_dir(suite));
    let mut names: Vec<&str> = ["Suite", "Codename"]
        .into_iter()
        .filter_map(|name| field(text, name))
        .collect();
    names.dedup();
    if !names.is_empty() && !names.contains(&suite) {
        return Err(MetaError::invalid(
            &file,
            format!(
                "is the Release of `{}`, not of `{suite}`",
                names.join("` and `")
            ),
        ));
    }
    if let Some(value) = field(text, "Valid-Until") {
        let until = DateTime::parse_from_rfc2822(value)
            .map(|d| d.to_utc())
            .or_else(|_| {
                chrono::NaiveDateTime::parse_from_str(value, "%a, %d %b %Y %H:%M:%S UTC")
                    .map(|d| d.and_utc())
            })
            .map_err(|_| MetaError::invalid(&file, format!("bad Valid-Until `{value}`")))?;
        if until < now {
            return Err(MetaError::invalid(
                &file,
                format!("expired on {value}: an old copy, or the upstream is not updated"),
            ));
        }
    }
    Ok(())
}

/// Our `Release` for `suite`: the upstream's descriptive fields, our date,
/// our components and architectures, and the SHA-256 of `files` only. No
/// `Valid-Until`, as a pinned snapshot must not expire, and no
/// `Acquire-By-Hash`, as `by-hash` paths are not served.
pub fn release_file(
    upstream: &str,
    suite: &str,
    components: &[String],
    architectures: &[String],
    files: &[Entry],
    date: DateTime<Utc>,
) -> String {
    let mut text = String::new();
    for name in KEPT_FIELDS {
        if let Some(value) = field(upstream, name) {
            text.push_str(&format!("{name}: {value}\n"));
        }
    }
    text.push_str(&format!(
        "Date: {}\nArchitectures: {}\nComponents: {}\nSHA256:\n",
        date.format("%a, %d %b %Y %H:%M:%S UTC"),
        architectures.join(" "),
        components.join(" "),
    ));
    let prefix = format!("{}/", apt::suite_dir(suite));
    let mut files: Vec<&Entry> = files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for entry in files {
        let path = entry.path.strip_prefix(&prefix).unwrap_or(&entry.path);
        text.push_str(&format!(
            " {} {:>16} {path}\n",
            entry.checksum.hex(),
            entry.size
        ));
    }
    text
}

/// Fills `building` with a snapshot of the working tree: hardlinks of its
/// files, with `.packages` and `.origin`. Signing is the caller's.
/// Blocking.
pub fn stage_snapshot(layout: &Layout, mirror: &Mirror, building: &Path) -> std::io::Result<()> {
    let work = layout.ondemand_work(mirror);
    if !work.join(PACKAGES).is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "this mirror has no on-demand sync to snapshot yet; sync it first",
        ));
    }
    if let Some(parent) = building.parent() {
        std::fs::create_dir_all(parent)?;
    }
    layout::remove_tree(building)?;
    layout::link_tree(&work, building, &[])?;
    for hidden in [PACKAGES, ORIGIN] {
        content::link(&work.join(hidden), &building.join(hidden))?;
    }
    Ok(())
}

/// The file a snapshot signs: `dists/<suite>/Release` for apt,
/// `repodata/repomd.xml` for dnf.
pub fn signed_file(mirror: &Mirror, snapshot: &Path) -> PathBuf {
    match mirror.family {
        Family::Apt => snapshot
            .join(apt::suite_dir(&mirror.distribution))
            .join("Release"),
        Family::Dnf => snapshot.join(dnf::REPOMD),
    }
}

#[cfg(test)]
mod tests {
    use monitorflow_common::mirror::repo::meta::Checksum;

    use super::*;

    #[test]
    fn our_release_lists_what_is_served_and_never_expires() {
        let upstream = "Origin: Debian\nLabel: Debian-Security\nSuite: stable-security\n\
            Version: 13\nCodename: trixie-security\nDate: Sun, 15 Nov 2026 21:25:23 UTC\n\
            Valid-Until: Mon, 23 Nov 2026 11:43:19 UTC\nAcquire-By-Hash: yes\n\
            No-Support-for-Architecture-all: Packages\nArchitectures: amd64 arm64\n\
            Components: updates/main updates/contrib\nDescription: Debian 13 - Security Updates\n\
            SHA256:\n 00 1 main/binary-arm64/Packages\n";
        let entry = |path: &str, c: char| Entry {
            path: format!("dists/trixie-security/{path}"),
            size: 42,
            checksum: Checksum::new(Algorithm::Sha256, &c.to_string().repeat(64)).unwrap(),
        };
        let date = DateTime::parse_from_rfc3339("2026-11-16T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let text = release_file(
            upstream,
            "trixie-security",
            &["main".into()],
            &["amd64".into()],
            &[
                entry("main/i18n/Translation-en.xz", 'b'),
                entry("main/binary-amd64/Packages.xz", 'a'),
            ],
            date,
        );
        assert_eq!(
            text,
            format!(
                "Origin: Debian\nLabel: Debian-Security\nSuite: stable-security\nVersion: 13\n\
                 Codename: trixie-security\nDescription: Debian 13 - Security Updates\n\
                 No-Support-for-Architecture-all: Packages\n\
                 Date: Mon, 16 Nov 2026 10:00:00 UTC\nArchitectures: amd64\nComponents: main\n\
                 SHA256:\n {}               42 main/binary-amd64/Packages.xz\n {}               42 main/i18n/Translation-en.xz\n",
                "a".repeat(64),
                "b".repeat(64)
            )
        );
        // What the relay and apt read back from it.
        let listed = apt::release("trixie-security", &text).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed[0].path,
            "dists/trixie-security/main/binary-amd64/Packages.xz"
        );
    }

    #[test]
    fn only_the_upstream_release_of_the_suite_asked_for_and_not_expired_is_taken() {
        let now = DateTime::parse_from_rfc3339("2026-11-19T10:00:00Z")
            .unwrap()
            .to_utc();
        let release = |suite: &str, codename: &str, until: &str| {
            format!("Origin: Ubuntu\nSuite: {suite}\nCodename: {codename}\n{until}SHA256:\n")
        };
        let fits = |text: &str, suite: &str| upstream_release_fits(text, suite, now);
        // Ubuntu names the release in Codename, Debian the archive in Suite.
        assert!(fits(&release("noble-updates", "noble", ""), "noble-updates").is_ok());
        assert!(fits(&release("oldstable", "bookworm", ""), "bookworm").is_ok());
        assert!(fits("Origin: Example\nSHA256:\n", "stable").is_ok());
        let other = fits(&release("noble-proposed", "noble", ""), "noble-updates").unwrap_err();
        assert!(
            other
                .to_string()
                .contains("`noble-proposed` and `noble`, not of `noble-updates`"),
            "{other}"
        );
        let valid = "Valid-Until: Mon, 23 Nov 2026 11:43:19 UTC\n";
        assert!(fits(
            &release("stable-security", "trixie-security", valid),
            "trixie-security"
        )
        .is_ok());
        let old = "Valid-Until: Sun, 15 Nov 2026 11:43:19 UTC\n";
        let expired = fits(
            &release("stable-security", "trixie-security", old),
            "trixie-security",
        );
        assert!(expired.unwrap_err().to_string().contains("expired on"));
        let numeric = "Valid-Until: Sun, 15 Nov 2026 11:43:19 +0000\n";
        assert!(fits(&release("a", "a", numeric), "a").is_err());
        assert!(fits(&release("a", "a", "Valid-Until: soon\n"), "a").is_err());
        let same = fits(&release("b", "b", ""), "a").unwrap_err().to_string();
        assert!(
            same.ends_with("is the Release of `b`, not of `a`"),
            "{same}"
        );
    }

    #[test]
    fn a_compressed_index_is_also_listed_decompressed() {
        let dir = tempfile::tempdir().unwrap();
        let text = b"Package: mf-fixture\nVersion: 1.0\n";
        // `text`, gzipped.
        let gzipped: &[u8] = &[
            31, 139, 8, 0, 0, 0, 0, 0, 2, 255, 11, 72, 76, 206, 78, 76, 79, 181, 82, 200, 77, 211,
            77, 203, 172, 40, 41, 45, 74, 229, 10, 75, 45, 42, 206, 204, 207, 179, 82, 48, 212, 51,
            224, 2, 0, 79, 27, 202, 67, 33, 0, 0, 0,
        ];
        let gz = dir.path().join("stored");
        std::fs::write(&gz, gzipped).unwrap();
        let mut hasher = Algorithm::Sha256.hasher();
        hasher.update(text);
        let entry = plain_entry("dists/f/main/binary-amd64/Packages.gz", &gz)
            .unwrap()
            .unwrap();
        assert_eq!(entry.path, "dists/f/main/binary-amd64/Packages");
        assert_eq!(entry.size, text.len() as u64);
        assert_eq!(entry.checksum, hasher.finish());
        // A plain index is listed once, as it is.
        assert_eq!(
            plain_entry("dists/f/main/binary-amd64/Packages", &gz).unwrap(),
            None
        );
    }

    #[test]
    fn the_minor_release_comes_from_the_general_section() {
        let rocky = "[header]\nversion = 1.2\n\n[general]\nfamily = Rocky Linux\nversion = 9.8\n";
        assert_eq!(treeinfo_minor(rocky), Some(8));
        assert_eq!(treeinfo_minor("[general]\nversion = 9\n"), None);
        assert_eq!(treeinfo_minor("[header]\nversion = 1.2\n"), None);
        assert_eq!(treeinfo_minor(""), None);
    }

    #[test]
    fn fields_are_read_whole_and_exactly() {
        let text = "Suite: noble\nSuiteX: no\nDescription: Ubuntu Noble 24.04\n";
        assert_eq!(field(text, "Suite"), Some("noble"));
        assert_eq!(field(text, "Description"), Some("Ubuntu Noble 24.04"));
        assert_eq!(field(text, "Label"), None);
    }
}
