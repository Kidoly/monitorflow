//! MSI files of the software catalogue (ADR-0188): stored once by
//! SHA-256 under `<mirror_dir>/software/`, where the mirror listener
//! serves them at `/software/<sha256>.msi` and relays copy them from on a
//! miss. Uploads are bounded per file and in all; a file nothing names
//! any more goes [`KEEP_UNREFERENCED`] later.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::body::Body;
use futures_util::StreamExt;
use monitorflow_common::software::{valid_product_code, valid_sha256, MAX_MSI_BYTES};
use sqlx::PgPool;
use tokio::io::AsyncWriteExt;

/// Directory under `mirror_dir`, and the URL path of the mirror listener.
pub const DIR: &str = monitorflow_common::mirror::serve::SOFTWARE_DIR;
/// Largest file: Windows Installer's own limit.
pub const MAX_FILE_BYTES: u64 = MAX_MSI_BYTES;
/// What the central server keeps of them in all.
pub const MAX_TOTAL_BYTES: u64 = 20 << 30;
/// How long a file nothing names stays: past a command's longest time to
/// live, 7 days, so a command queued before its entry went still finds it.
pub const KEEP_UNREFERENCED: Duration = Duration::from_secs(8 * 24 * 3600);
/// How often files nothing names are looked for.
pub const SWEEP_EVERY: Duration = Duration::from_secs(3600);
/// Temporary files older than this are left over by a stopped server.
const STALE_TEMP: Duration = Duration::from_secs(24 * 3600);
const TEMP_PREFIX: &str = ".upload-";

/// `<mirror_dir>/software`.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

/// Why an upload is refused.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("the file is empty")]
    Empty,
    #[error("the file is larger than 2 GiB, Windows Installer's limit")]
    TooLarge,
    #[error("the upload stopped: {0}")]
    Cut(String),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// A file received into the store's temporary area.
#[derive(Debug, Clone)]
pub struct Received {
    pub temp: PathBuf,
    pub sha256: String,
    pub size: u64,
}

impl Store {
    pub fn in_mirror_dir(mirror_dir: &Path) -> Self {
        Self {
            dir: mirror_dir.join(DIR),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the file of SHA-256 `sha256` is kept.
    pub fn path(&self, sha256: &str) -> PathBuf {
        self.dir.join(file_name(sha256))
    }

    fn temp_path(&self) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        self.dir
            .join(format!("{TEMP_PREFIX}{}-{n}", std::process::id()))
    }

    /// Writes `body` to a temporary file of the store, hashing it, and
    /// refuses more than [`MAX_FILE_BYTES`]. The caller keeps or removes
    /// the file.
    pub async fn receive(&self, body: Body) -> Result<Received, UploadError> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let temp = self.temp_path();
        let result = self.write(&temp, body).await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(&temp).await;
        }
        result
    }

    async fn write(&self, temp: &Path, body: Body) -> Result<Received, UploadError> {
        let mut file = tokio::fs::File::create(temp).await?;
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        let mut size = 0u64;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| UploadError::Cut(e.to_string()))?;
            size += chunk.len() as u64;
            if size > MAX_FILE_BYTES {
                return Err(UploadError::TooLarge);
            }
            context.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        if size == 0 {
            return Err(UploadError::Empty);
        }
        Ok(Received {
            temp: temp.to_owned(),
            sha256: hex(context.finish().as_ref()),
            size,
        })
    }

    /// Moves a received file into place, read-only. Blocking.
    pub fn keep(&self, received: &Received) -> io::Result<()> {
        let dest = self.path(&received.sha256);
        let mut permissions = std::fs::metadata(&received.temp)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&received.temp, permissions)?;
        std::fs::rename(&received.temp, dest)
    }
}

/// `<sha256>.msi`.
pub fn file_name(sha256: &str) -> String {
    format!("{sha256}.msi")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the `Property` table of an MSI file says of its product.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Product {
    /// Upper case; `None` when the file has none, or not a GUID.
    pub product_code: Option<String>,
    pub product_name: String,
    pub product_version: String,
    pub manufacturer: String,
}

/// Longest product property kept.
const MAX_PROPERTY: usize = 255;

/// Reads `path` as a Windows Installer package. Blocking.
pub fn read_product(path: &Path) -> Result<Product, String> {
    let not_msi = |e: io::Error| format!("the file is not a Windows Installer package: {e}");
    let mut package = msi::open(path).map_err(not_msi)?;
    if !package.has_table("Property") {
        return Err("the file is not a Windows Installer package: it has no Property table".into());
    }
    let mut product = Product::default();
    let rows = package
        .select_rows(msi::Select::table("Property"))
        .map_err(not_msi)?;
    for row in rows {
        let (Some(name), Some(value)) = (row[0].as_str(), row[1].as_str()) else {
            continue;
        };
        let value: String = value
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_PROPERTY)
            .collect();
        match name {
            "ProductCode" => {
                let code = value.to_ascii_uppercase();
                product.product_code = valid_product_code(&code).then_some(code);
            }
            "ProductName" => product.product_name = value,
            "ProductVersion" => product.product_version = value,
            "Manufacturer" => product.manufacturer = value,
            _ => {}
        }
    }
    Ok(product)
}

/// What one sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Swept {
    pub removed: u64,
    pub freed_bytes: u64,
}

/// SQL true when the file `f` is named by a catalogue entry or a campaign.
const REFERENCED: &str = "(EXISTS (SELECT 1 FROM software_catalog c WHERE c.file_sha256 = f.sha256)
     OR EXISTS (SELECT 1 FROM campaigns k WHERE k.software->'packages' @>
         jsonb_build_array(jsonb_build_object('msi', jsonb_build_object('sha256', f.sha256)))))";

/// Marks the files nothing names, removes those nothing named for
/// [`KEEP_UNREFERENCED`], then files of the directory without a row and
/// stale uploads.
pub async fn sweep(pool: &PgPool, store: &Store) -> Result<Swept, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // SQL-safe: interpolates only the constant REFERENCED.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE software_files f SET unreferenced_since = NULL
         WHERE unreferenced_since IS NOT NULL AND {REFERENCED}"
    )))
    .execute(&mut *tx)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE software_files f SET unreferenced_since = now()
         WHERE unreferenced_since IS NULL AND NOT {REFERENCED}"
    )))
    .execute(&mut *tx)
    .await?;
    let gone: Vec<(String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "DELETE FROM software_files f
         WHERE unreferenced_since < now() - make_interval(secs => $1) AND NOT {REFERENCED}
         RETURNING sha256, size"
    )))
    .bind(KEEP_UNREFERENCED.as_secs_f64())
    .fetch_all(&mut *tx)
    .await?;
    let known: Vec<String> = sqlx::query_scalar("SELECT sha256 FROM software_files")
        .fetch_all(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut swept = Swept::default();
    for (sha256, size) in &gone {
        match std::fs::remove_file(store.path(sha256)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%error, sha256, "cannot remove an MSI file");
                continue;
            }
        }
        swept.removed += 1;
        swept.freed_bytes += u64::try_from(*size).unwrap_or(0);
    }
    let dir = store.dir.clone();
    let orphans = tokio::task::spawn_blocking(move || orphans(&dir, &known))
        .await
        .unwrap_or_else(|e| Err(io::Error::other(e)));
    match orphans {
        Ok(freed) => swept.freed_bytes += freed,
        Err(error) => tracing::warn!(%error, "cannot look for MSI files without a row"),
    }
    Ok(swept)
}

/// Removes files of `dir` that are neither a known file nor a recent
/// upload; returns the bytes freed. Files younger than an hour stay, so
/// an upload being committed is not taken for an orphan. Blocking.
fn orphans(dir: &Path, known: &[String]) -> io::Result<u64> {
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        other => other?,
    };
    let mut freed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let meta = entry.metadata()?;
        let age = meta.modified()?.elapsed().unwrap_or_default();
        let temp = name.starts_with(TEMP_PREFIX);
        let stored = name
            .strip_suffix(".msi")
            .is_some_and(|sha| valid_sha256(sha) && known.iter().any(|k| k == sha));
        let old = if temp {
            age > STALE_TEMP
        } else {
            age > Duration::from_secs(3600)
        };
        if meta.is_file() && !stored && old {
            std::fs::remove_file(entry.path())?;
            freed += meta.len();
        }
    }
    Ok(freed)
}

/// Sweeps every [`SWEEP_EVERY`] until the process ends.
pub fn spawn_sweeper(pool: PgPool, store: Store) {
    tokio::spawn(async move {
        loop {
            match sweep(&pool, &store).await {
                Ok(swept) if swept.removed > 0 || swept.freed_bytes > 0 => {
                    tracing::info!(
                        files = swept.removed,
                        freed_bytes = swept.freed_bytes,
                        "MSI files nothing names were removed"
                    )
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "cannot sweep the MSI files"),
            }
            tokio::time::sleep(SWEEP_EVERY).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal package with a Property table, as the `msi` crate writes it.
    pub(crate) fn package(properties: &[(&str, &str)]) -> Vec<u8> {
        let mut package =
            msi::Package::create(msi::PackageType::Installer, io::Cursor::new(Vec::new())).unwrap();
        package
            .create_table(
                "Property",
                vec![
                    msi::Column::build("Property").primary_key().id_string(72),
                    msi::Column::build("Value").nullable().text_string(0),
                ],
            )
            .unwrap();
        let rows = properties
            .iter()
            .map(|(k, v)| vec![msi::Value::from(*k), msi::Value::from(*v)])
            .collect();
        package
            .insert_rows(msi::Insert::into("Property").rows(rows))
            .unwrap();
        package.into_inner().unwrap().into_inner()
    }

    #[test]
    fn products_are_read_from_the_property_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.msi");
        std::fs::write(
            &path,
            package(&[
                ("ProductCode", "{6f1c2e7a-3b9d-4c55-a0e2-8d4f7b1a9c30}"),
                ("ProductName", "Tiny\u{7}"),
                ("ProductVersion", "1.0.0"),
                ("Manufacturer", "Example"),
                ("MARKER", "none"),
            ]),
        )
        .unwrap();
        assert_eq!(
            read_product(&path).unwrap(),
            Product {
                product_code: Some("{6F1C2E7A-3B9D-4C55-A0E2-8D4F7B1A9C30}".into()),
                product_name: "Tiny".into(),
                product_version: "1.0.0".into(),
                manufacturer: "Example".into(),
            }
        );
        std::fs::write(&path, package(&[("ProductCode", "not a guid")])).unwrap();
        assert_eq!(read_product(&path).unwrap().product_code, None);
        std::fs::write(&path, b"MZ, not an MSI").unwrap();
        assert!(read_product(&path)
            .unwrap_err()
            .contains("not a Windows Installer"));
    }

    #[tokio::test]
    async fn uploads_are_hashed_bounded_and_kept_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::in_mirror_dir(dir.path());
        let received = store.receive(Body::from("hello")).await.unwrap();
        assert_eq!(received.size, 5);
        assert_eq!(
            received.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        store.keep(&received).unwrap();
        let kept = store.path(&received.sha256);
        assert!(std::fs::metadata(&kept).unwrap().permissions().readonly());
        assert!(matches!(
            store.receive(Body::empty()).await,
            Err(UploadError::Empty)
        ));
        // Nothing is left of a refused upload.
        let left: Vec<_> = std::fs::read_dir(store.dir()).unwrap().collect();
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn orphans_are_old_files_without_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let known = "a".repeat(64);
        let unknown = "b".repeat(64);
        for name in [
            file_name(&known),
            file_name(&unknown),
            format!("{TEMP_PREFIX}1-0"),
        ] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        // Recent: everything stays.
        assert_eq!(
            orphans(dir.path(), std::slice::from_ref(&known)).unwrap(),
            0
        );
        let old = std::time::SystemTime::now() - Duration::from_secs(2 * 24 * 3600);
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let file = std::fs::File::options()
                .write(true)
                .open(entry.unwrap().path())
                .unwrap();
            file.set_modified(old).unwrap();
        }
        assert_eq!(
            orphans(dir.path(), std::slice::from_ref(&known)).unwrap(),
            2
        );
        let left: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, [file_name(&known)]);
    }
}
