//! The plain HTTP mirror listener of the server (ADR-0030) and of relays
//! (ADR-0034), with the fixed layout relays and agents rely on:
//!
//! - `/apt/<mirror>/<snapshot>/` and `/apt/<mirror>/current/`
//! - `/dnf/<mirror>/<snapshot>/` and `/dnf/<mirror>/current/`
//! - `/keys/monitorflow-archive.asc`
//! - `/agent/<version>/`: signed agent releases for self-updates (ADR-0053)
//! - `/software/<sha256>.msi`: MSI files of the software catalogue, which
//!   the signed `software` command names by digest (ADR-0188); a relay
//!   fetches one it lacks from its upstream
//! - `/index.json`, see [`Index`]
//!
//! GET and HEAD only, no directory listings, and any path segment
//! starting with a dot is 404, which hides snapshots being built. Clients
//! check what they download by its signatures, not by this transport.
//!
//! Links are followed only inside the served directory they are in: the
//! tree is written by tools that parse what they download, and the
//! listener's process can read more than it serves. A `current` link
//! resolves inside and works; a link out, or a served directory that is
//! itself a link, is 404.
//!
//! A file missing from an on-demand snapshot (ADR-0142) that its package
//! list names is served from the content store, fetched first if needed,
//! by [`OnDemand`]: 502 when the upstream fails or sends something else,
//! 404 when nobody has it.

use std::future::Future;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tower_http::services::ServeFile;

use super::demand::{DemandError, OnDemand};
use super::{
    is_mirror_name, is_snapshot_name, read_current, Family, Index, CURRENT, INDEX_PATH, PUBLIC_KEY,
};

/// Directory and URL path of the agent releases.
pub const AGENT_DIR: &str = "agent";
/// Directory and URL path of the MSI files of the software catalogue.
pub const SOFTWARE_DIR: &str = "software";

/// The served directories: one per family, and the public key file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// The directory holding all the others, which nothing less trusted
    /// than the listener can replace: the mirror directory, a volume's
    /// mount point in the compose deployment. Links are resolved against
    /// it, so a served directory below it must not be a link.
    pub base: PathBuf,
    pub apt: PathBuf,
    pub dnf: PathBuf,
    pub public_key: PathBuf,
    /// Agent releases, one directory per version (ADR-0053).
    pub agent: PathBuf,
    /// MSI files by SHA-256 (ADR-0188): uploaded on the server, absent on
    /// relays, which keep what they fetch in the on-demand store.
    pub software: PathBuf,
}

impl Served {
    /// The relay's layout: `apt/`, `dnf/` and `keys/` under `root`.
    pub fn under(root: &Path) -> Self {
        Self {
            base: root.to_owned(),
            apt: root.join(Family::Apt.as_str()),
            dnf: root.join(Family::Dnf.as_str()),
            public_key: root.join("keys").join(PUBLIC_KEY),
            agent: root.join(AGENT_DIR),
            software: root.join(SOFTWARE_DIR),
        }
    }

    pub fn root(&self, family: Family) -> &Path {
        match family {
            Family::Apt => &self.apt,
            Family::Dnf => &self.dnf,
        }
    }
}

/// The routes over `served`.
pub fn router(served: &Served) -> Router {
    router_with(served, None)
}

/// The routes over `served`, fetching the packages of on-demand snapshots
/// with `ondemand` (ADR-0142).
pub fn router_with(served: &Served, ondemand: Option<OnDemand>) -> Router {
    let dir = |root: &Path, family: Option<Family>| {
        get(file).with_state(Tree {
            base: served.base.clone(),
            root: root.to_owned(),
            fixed: None,
            family,
            ondemand: family.and(ondemand.clone()),
        })
    };
    let keys = served.public_key.parent().unwrap_or(Path::new(""));
    Router::new()
        .route("/apt/{*path}", dir(&served.apt, Some(Family::Apt)))
        .route("/dnf/{*path}", dir(&served.dnf, Some(Family::Dnf)))
        .route(&format!("/{AGENT_DIR}/{{*path}}"), dir(&served.agent, None))
        .route(
            &format!("/keys/{PUBLIC_KEY}"),
            get(file).with_state(Tree {
                base: served.base.clone(),
                root: keys.to_owned(),
                fixed: Some(PUBLIC_KEY),
                family: None,
                ondemand: None,
            }),
        )
        .route(
            &format!("/{SOFTWARE_DIR}/{{name}}"),
            get(software).with_state(Software {
                tree: Tree {
                    base: served.base.clone(),
                    root: served.software.clone(),
                    fixed: None,
                    family: None,
                    ondemand: None,
                },
                ondemand: ondemand.clone(),
            }),
        )
        .route(INDEX_PATH, get(index).with_state(served.clone()))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn(guard))
}

/// One served directory, or its only served file when `fixed`.
#[derive(Debug, Clone)]
struct Tree {
    base: PathBuf,
    root: PathBuf,
    fixed: Option<&'static str>,
    /// The family of a package tree.
    family: Option<Family>,
    /// Fetches what on-demand snapshots of this tree miss.
    ondemand: Option<OnDemand>,
}

/// A file checked to be inside its served directory.
struct Contained {
    /// What to open: on Linux the checked file itself, through its
    /// descriptor, so nothing swapped in afterwards is served.
    path: PathBuf,
    _open: Option<std::fs::File>,
}

impl Tree {
    /// `root` with its links resolved, when none is below `base`.
    fn real_root(&self) -> Option<PathBuf> {
        let below = self.root.strip_prefix(&self.base).ok()?;
        Some(std::fs::canonicalize(&self.base).ok()?.join(below))
    }

    /// The file `relative` names, when it is a regular file that resolves
    /// inside the served directory. Blocking.
    fn contained(&self, relative: &Path) -> Option<Contained> {
        let root = self.real_root()?;
        let path = self.root.join(relative);
        if !std::fs::metadata(&path).ok()?.is_file() {
            return None;
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let file = std::fs::File::open(&path).ok()?;
            let link = PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()));
            // Without `/proc`, the check below, on the path.
            if let Ok(real) = std::fs::read_link(&link) {
                let inside = real.starts_with(&root) && file.metadata().ok()?.is_file();
                return inside.then_some(Contained {
                    path: link,
                    _open: Some(file),
                });
            }
        }
        let real = std::fs::canonicalize(&path).ok()?;
        real.starts_with(&root).then_some(Contained {
            path: real,
            _open: None,
        })
    }
}

/// The path below a served directory that the URL path `rest` names;
/// `None` for anything but plain names between single slashes.
fn relative(rest: &str) -> Option<PathBuf> {
    let rest = percent_encoding::percent_decode_str(rest)
        .decode_utf8()
        .ok()?;
    let mut path = PathBuf::new();
    for segment in rest.split('/') {
        let mut parts = Path::new(segment).components();
        match (parts.next(), parts.next()) {
            (Some(std::path::Component::Normal(name)), None)
                if name == segment && !segment.contains(['\\', '\0']) =>
            {
                path.push(name)
            }
            _ => return None,
        }
    }
    Some(path)
}

async fn file(State(tree): State<Tree>, request: Request) -> Response {
    let named = match (tree.fixed, request.uri().path().splitn(3, '/').nth(2)) {
        (Some(name), _) => Some(PathBuf::from(name)),
        (None, Some(rest)) => relative(rest),
        (None, None) => None,
    };
    let Some(named) = named else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&named).first_or_octet_stream();
    let (checked, wanted) = (tree.clone(), named.clone());
    let found = tokio::task::spawn_blocking(move || checked.contained(&wanted)).await;
    let found = match found {
        Ok(Some(found)) => found,
        _ => match on_demand(&tree, &named).await {
            Ok(Some(stored)) => Contained {
                path: stored,
                _open: None,
            },
            Ok(None) | Err(DemandError::Missing) => return StatusCode::NOT_FOUND.into_response(),
            Err(DemandError::Upstream(_)) => return StatusCode::BAD_GATEWAY.into_response(),
            Err(DemandError::Local(error)) => {
                tracing::warn!(%error, "cannot serve an on-demand package");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        },
    };
    // `found` stays open until the file is opened again through it.
    match ServeFile::new_with_mime(&found.path, &mime)
        .try_call(request)
        .await
    {
        Ok(response) => response.map(Body::new),
        Err(error) => {
            tracing::warn!(%error, "cannot serve a mirror file");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

/// The stored file of `named`, `<mirror>/<snapshot>/<path>` below a
/// package tree, when its snapshot is on demand and lists the path.
async fn on_demand(tree: &Tree, named: &Path) -> Result<Option<PathBuf>, DemandError> {
    let (Some(family), Some(ondemand)) = (tree.family, &tree.ondemand) else {
        return Ok(None);
    };
    let Some(named) = named.to_str() else {
        return Ok(None);
    };
    let mut parts = named.splitn(3, '/');
    let (Some(mirror), Some(snapshot), Some(rest)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(None);
    };
    if !is_mirror_name(mirror) {
        return Ok(None);
    }
    let (checked, mirror_name, asked) = (tree.clone(), mirror.to_owned(), snapshot.to_owned());
    // The real snapshot, `current` resolved, when it is a directory that
    // resolves inside the served one.
    let resolved = tokio::task::spawn_blocking(move || {
        let dir = checked.root.join(&mirror_name);
        let name = if asked == CURRENT {
            read_current(&dir)?
        } else {
            asked
        };
        if !is_snapshot_name(&name) {
            return None;
        }
        let real = std::fs::canonicalize(dir.join(&name)).ok()?;
        (real.starts_with(checked.real_root()?) && real.is_dir()).then_some((name, real))
    })
    .await
    .map_err(|e| DemandError::Local(e.to_string()))?;
    let Some((snapshot, dir)) = resolved else {
        return Ok(None);
    };
    ondemand
        .package(family, mirror, &snapshot, &dir, rest)
        .await
}

/// The MSI files and, on a relay, what fetches those it lacks.
#[derive(Debug, Clone)]
struct Software {
    tree: Tree,
    ondemand: Option<OnDemand>,
}

/// `/software/<sha256>.msi`: the uploaded file, else on a relay the one
/// the on-demand store holds or fetches, checked against the digest.
async fn software(
    State(software): State<Software>,
    axum::extract::Path(name): axum::extract::Path<String>,
    request: Request,
) -> Response {
    let Some(sha256) = name
        .strip_suffix(".msi")
        .filter(|h| crate::software::valid_sha256(h))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (tree, wanted) = (software.tree.clone(), PathBuf::from(&name));
    let found = tokio::task::spawn_blocking(move || tree.contained(&wanted)).await;
    let found = match found {
        Ok(Some(found)) => found,
        _ => {
            let Some(ondemand) = &software.ondemand else {
                return StatusCode::NOT_FOUND.into_response();
            };
            match ondemand.software(sha256).await {
                Ok(Some(path)) => Contained { path, _open: None },
                Ok(None) | Err(DemandError::Missing) => {
                    return StatusCode::NOT_FOUND.into_response()
                }
                Err(DemandError::Upstream(_)) => return StatusCode::BAD_GATEWAY.into_response(),
                Err(DemandError::Local(error)) => {
                    tracing::warn!(%error, "cannot serve an MSI file");
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        }
    };
    let mime = mime_guess::from_path(&name).first_or_octet_stream();
    match ServeFile::new_with_mime(&found.path, &mime)
        .try_call(request)
        .await
    {
        Ok(response) => response.map(Body::new),
        Err(error) => {
            tracing::warn!(%error, "cannot serve an MSI file");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn index(State(served): State<Served>) -> Response {
    match tokio::task::spawn_blocking(move || Index::scan(&served)).await {
        Ok(Ok(index)) => (
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_vec(&index).unwrap_or_default(),
        )
            .into_response(),
        Ok(Err(error)) => {
            tracing::warn!(%error, "cannot list the published mirrors");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn guard(request: Request, next: Next) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    // Decoded first: `%2e` is a dot too.
    let path = percent_encoding::percent_decode_str(request.uri().path()).decode_utf8_lossy();
    if path
        .split('/')
        .any(|s| s.starts_with('.') || s.contains('\\'))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

/// Serves [`router`] on `listener` until `shutdown` resolves.
pub async fn serve(
    listener: TcpListener,
    served: Served,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_with(listener, served, None, shutdown).await
}

/// Serves [`router_with`] on `listener` until `shutdown` resolves.
pub async fn serve_with(
    listener: TcpListener,
    served: Served,
    ondemand: Option<OnDemand>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router_with(&served, ondemand))
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_names_make_a_path() {
        for (rest, path) in [
            ("debian/s1/InRelease", "debian/s1/InRelease"),
            ("a_1.0%2Bb1%7Ex.deb", "a_1.0+b1~x.deb"),
        ] {
            assert_eq!(relative(rest), Some(PathBuf::from(path)), "{rest}");
        }
        for rest in [
            "",
            "a//b",
            "a/",
            "/a",
            "a/../b",
            "a/./b",
            "a/%2e%2e/b",
            "a%2F%2Fb",
            "a%5Cb",
            "a%00b",
            "a/%ff",
        ] {
            assert_eq!(relative(rest), None, "{rest}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn links_are_followed_inside_the_served_directory_only() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("mirrors");
        let served = Served::under(&base);
        std::fs::create_dir_all(served.dnf.join("rocky/s1")).unwrap();
        std::fs::create_dir_all(dir.path().join("ca")).unwrap();
        std::fs::write(served.dnf.join("rocky/s1/repomd.xml"), "x").unwrap();
        std::fs::write(dir.path().join("ca/key.pem"), "secret").unwrap();
        std::fs::write(base.join("private"), "secret").unwrap();
        let s1 = served.dnf.join("rocky/s1");
        symlink("s1", served.dnf.join("rocky/current")).unwrap();
        symlink("repomd.xml", s1.join("inside")).unwrap();
        symlink("../../../../ca/key.pem", s1.join("relative")).unwrap();
        symlink(dir.path().join("ca/key.pem"), s1.join("absolute")).unwrap();
        symlink(dir.path().join("ca"), s1.join("directory")).unwrap();
        symlink("../../../private", s1.join("sibling")).unwrap();
        symlink("missing", s1.join("dangling")).unwrap();
        let tree = Tree {
            base: base.clone(),
            root: served.dnf.clone(),
            fixed: None,
            family: None,
            ondemand: None,
        };
        let read = |tree: &Tree, path: &str| {
            let found = tree.contained(Path::new(path))?;
            Some(std::fs::read_to_string(&found.path).unwrap())
        };
        for path in [
            "rocky/s1/repomd.xml",
            "rocky/current/repomd.xml",
            "rocky/s1/inside",
        ] {
            assert_eq!(read(&tree, path).as_deref(), Some("x"), "{path}");
        }
        for path in [
            "rocky/s1/relative",
            "rocky/s1/absolute",
            "rocky/s1/directory/key.pem",
            "rocky/s1/sibling",
            "rocky/s1/dangling",
            "rocky/s1/missing",
            "rocky/s1",
        ] {
            assert_eq!(read(&tree, path), None, "{path}");
        }

        // A served directory that is a link serves nothing, wherever it
        // points; nor does one outside the base.
        std::fs::create_dir_all(base.join("aptly")).unwrap();
        symlink(dir.path().join("ca"), base.join("aptly/public")).unwrap();
        let linked = Tree {
            base: base.clone(),
            root: base.join("aptly/public"),
            fixed: None,
            family: None,
            ondemand: None,
        };
        assert_eq!(read(&linked, "key.pem"), None);
        let outside = Tree {
            base,
            root: dir.path().join("ca"),
            fixed: None,
            family: None,
            ondemand: None,
        };
        assert_eq!(read(&outside, "key.pem"), None);
    }
}
