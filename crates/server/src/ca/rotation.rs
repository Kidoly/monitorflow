//! Change of certificate authority on disk (ADR-0088).
//!
//! The active generation stays in `ca-key.pem` and `ca-cert.pem`, so a
//! server that never rotates keeps the layout of ADR-0004. A change adds:
//!
//! - `rotation.json`: the [`Rotation`] under way;
//! - `next/`: the new generation while it is only trusted ([`Stage::Introduced`]);
//! - `previous/`: the old one once the new one issues ([`Stage::Switched`]);
//! - `retired/<sha256>.json`: each retired generation's certificate, never
//!   its key. A restore keeps this directory (ADR-0058), so a server
//!   restored from an older backup says that its CA was retired.
//!
//! Each step writes its files in an order that [`recover_and_load`] can
//! finish or undo after a crash.

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::sync::{Arc, PoisonError};
use std::time::{Duration, SystemTime};

use monitorflow_common::tls::{fingerprint_hex, Fingerprint};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::{
    create_named, load_generation, now, store, CaError, CaOptions, CertificateAuthority,
    Generation, CERT_FILE, KEY_FILE,
};

pub const ROTATION_FILE: &str = "rotation.json";
pub const NEXT_DIR: &str = "next";
pub const PREVIOUS_DIR: &str = "previous";
pub const RETIRED_DIR: &str = "retired";
/// How long an emergency change trusts the old CA, for peers to move.
pub const EMERGENCY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Peers learn to trust the new CA; the old one still issues.
    Introduced,
    /// The new CA issues; peers still holding a certificate of the old one
    /// get a new certificate.
    Switched,
}

/// Content of `rotation.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rotation {
    pub stage: Stage,
    /// After a suspected compromise: no introduction, and the old CA stops
    /// being trusted at `distrust_at` whoever has moved.
    pub emergency: bool,
    /// Unix seconds.
    pub started_at: i64,
    pub switched_at: Option<i64>,
    pub distrust_at: Option<i64>,
}

/// Content of `retired/<sha256>.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Retired {
    /// Lowercase hexadecimal SHA-256 of the DER certificate.
    pub fingerprint: String,
    pub retired_at: i64,
    pub emergency: bool,
    pub cert_pem: String,
}

/// A CA certificate, for the status page.
#[derive(Debug, Clone)]
pub struct CaInfo {
    pub fingerprint: Fingerprint,
    pub cert_pem: String,
    pub not_before: SystemTime,
    pub not_after: SystemTime,
}

#[derive(Debug, Clone)]
pub struct CaStatus {
    pub active: CaInfo,
    /// The new CA while introduced, the old one once switched.
    pub other: Option<CaInfo>,
    pub rotation: Option<Rotation>,
    /// Set when the active CA is one this server retired: a restore of an
    /// older backup brought it back.
    pub restored_retired: Option<Retired>,
}

pub(super) struct RotationState {
    pub active: Arc<Generation>,
    pub other: Option<Arc<Generation>>,
    pub rotation: Option<Rotation>,
    retired: Vec<Retired>,
}

impl RotationState {
    /// The active generation, then the other one.
    pub fn trusted(&self) -> impl Iterator<Item = &Arc<Generation>> {
        std::iter::once(&self.active).chain(self.other.iter())
    }

    pub fn status(&self) -> CaStatus {
        let active = fingerprint_hex(&self.active.fingerprint);
        CaStatus {
            active: self.active.info(),
            other: self.other.as_ref().map(|g| g.info()),
            rotation: self.rotation.clone(),
            restored_retired: self
                .retired
                .iter()
                .find(|r| r.fingerprint == active)
                .cloned(),
        }
    }
}

fn read_rotation(dir: &Path) -> Result<Option<Rotation>, CaError> {
    let path = dir.join(ROTATION_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| CaError::corrupt(&path, e)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(CaError::io(&path, error)),
    }
}

fn write_rotation(dir: &Path, rotation: &Rotation) -> Result<(), CaError> {
    let path = dir.join(ROTATION_FILE);
    let json = serde_json::to_vec_pretty(rotation).map_err(|e| CaError::corrupt(&path, e))?;
    store::write_atomic(&path, &json, 0o644)
}

fn remove_file(path: &Path) -> Result<(), CaError> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(CaError::io(path, error)),
        _ => Ok(()),
    }
}

fn remove_dir(path: &Path) -> Result<(), CaError> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(CaError::io(path, error)),
        _ => Ok(()),
    }
}

fn exists(path: &Path) -> Result<bool, CaError> {
    path.try_exists().map_err(|e| CaError::io(path, e))
}

fn create_dir(dir: &Path) -> Result<(), CaError> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| CaError::io(dir, e))
}

/// Copies the key, then the certificate, of `from` over those of `to`.
fn copy_generation(from: &Path, to: &Path) -> Result<(), CaError> {
    create_dir(to)?;
    for (name, mode) in [(KEY_FILE, 0o600), (CERT_FILE, 0o644)] {
        let source = from.join(name);
        let bytes = fs::read(&source).map_err(|e| CaError::io(&source, e))?;
        store::write_atomic(&to.join(name), &bytes, mode)?;
    }
    Ok(())
}

fn read_retired(dir: &Path) -> Result<Vec<Retired>, CaError> {
    let dir = dir.join(RETIRED_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(CaError::io(&dir, error)),
    };
    let mut retired = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| CaError::io(&dir, e))?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let bytes = fs::read(&path).map_err(|e| CaError::io(&path, e))?;
            retired.push(serde_json::from_slice(&bytes).map_err(|e| CaError::corrupt(&path, e))?);
        }
    }
    retired.sort_by_key(|r: &Retired| r.retired_at);
    Ok(retired)
}

/// Finishes or undoes a step a crash interrupted, then loads both
/// generations.
pub(super) fn recover_and_load(dir: &Path, options: &CaOptions) -> Result<RotationState, CaError> {
    let (next, previous) = (dir.join(NEXT_DIR), dir.join(PREVIOUS_DIR));
    let rotation = read_rotation(dir)?;
    match rotation.as_ref().map(|r| r.stage) {
        // A start that did not write `rotation.json`, or a retirement that
        // removed it: neither directory is in use.
        None => {
            remove_dir(&next)?;
            remove_dir(&previous)?;
        }
        Some(Stage::Introduced) => {
            // A switch that stopped before `rotation.json` said so.
            remove_dir(&previous)?;
            if !exists(&next.join(CERT_FILE))? {
                warn!(dir = %next.display(), "the new certificate authority is incomplete; creating it again");
                remove_dir(&next)?;
                create_named(&next, &next_name(options), options.ca_validity)?;
            }
        }
        Some(Stage::Switched) => {
            if exists(&next)? {
                info!("finishing an interrupted change of certificate authority");
                copy_generation(&next, dir)?;
                remove_dir(&next)?;
            }
        }
    }
    let active = Arc::new(load_generation(dir)?);
    let other = match rotation.as_ref().map(|r| r.stage) {
        None => None,
        Some(Stage::Introduced) => Some(Arc::new(load_generation(&next)?)),
        Some(Stage::Switched) => Some(Arc::new(load_generation(&previous)?)),
    };
    let retired = read_retired(dir)?;
    let state = RotationState {
        active,
        other,
        rotation,
        retired,
    };
    if let Some(retired) = state.status().restored_retired {
        tracing::error!(
            fingerprint = %retired.fingerprint,
            "this server's certificate authority was retired: the server was restored from a backup taken before the change, and servers and relays that moved cannot connect"
        );
    }
    Ok(state)
}

/// The common name of a new generation: the default name and the date, so
/// that two generations never share a subject.
fn next_name(options: &CaOptions) -> String {
    let today = now().date();
    format!("{} {today}", options.common_name)
}

impl CertificateAuthority {
    /// The current change of certificate authority, if any.
    pub fn rotation(&self) -> Option<Rotation> {
        self.state().rotation.clone()
    }

    /// Starts a change of certificate authority: creates a new CA that
    /// peers learn to trust. With `emergency`, it issues at once and the
    /// old CA stops being trusted after [`EMERGENCY_WINDOW`]; an emergency
    /// also cuts short a planned change that has not switched yet.
    pub fn start_rotation(&self, emergency: bool) -> Result<Rotation, CaError> {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        let next = self.dir.join(NEXT_DIR);
        match state.rotation.as_ref().map(|r| r.stage) {
            None => {
                remove_dir(&next)?;
                create_named(&next, &next_name(&self.options), self.options.ca_validity)?;
                let rotation = Rotation {
                    stage: Stage::Introduced,
                    emergency: false,
                    started_at: now().unix_timestamp(),
                    switched_at: None,
                    distrust_at: None,
                };
                write_rotation(&self.dir, &rotation)?;
                state.other = Some(Arc::new(load_generation(&next)?));
                state.rotation = Some(rotation);
                info!("started a change of certificate authority");
            }
            Some(Stage::Introduced) if emergency => {}
            Some(_) => {
                return Err(CaError::InvalidRequest(
                    "a change of certificate authority is already under way".into(),
                ))
            }
        }
        if emergency {
            self.switch_locked(&mut state, true)?;
        }
        Ok(state.rotation.clone().expect("set above"))
    }

    /// Makes the introduced CA the one that issues and signs; the old one
    /// stays trusted until [`Self::retire`].
    pub fn switch(&self) -> Result<Rotation, CaError> {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        self.switch_locked(&mut state, false)?;
        Ok(state.rotation.clone().expect("switched"))
    }

    fn switch_locked(&self, state: &mut RotationState, emergency: bool) -> Result<(), CaError> {
        let Some(rotation) = state
            .rotation
            .clone()
            .filter(|r| r.stage == Stage::Introduced)
        else {
            return Err(CaError::InvalidRequest(
                "no new certificate authority is waiting to take over".into(),
            ));
        };
        let (next, previous) = (self.dir.join(NEXT_DIR), self.dir.join(PREVIOUS_DIR));
        remove_dir(&previous)?;
        copy_generation(&self.dir, &previous)?;
        let now = now().unix_timestamp();
        let rotation = Rotation {
            stage: Stage::Switched,
            emergency: rotation.emergency || emergency,
            switched_at: Some(now),
            distrust_at: (rotation.emergency || emergency)
                .then(|| now + EMERGENCY_WINDOW.as_secs() as i64),
            ..rotation
        };
        // From here, a crash is finished at the next start.
        write_rotation(&self.dir, &rotation)?;
        copy_generation(&next, &self.dir)?;
        remove_dir(&next)?;
        let old = std::mem::replace(&mut state.active, Arc::new(load_generation(&self.dir)?));
        state.other = Some(old);
        state.rotation = Some(rotation);
        info!(
            emergency = rotation_emergency(state),
            "the new certificate authority now issues certificates"
        );
        Ok(())
    }

    /// Stops trusting the old CA and destroys its key; its certificate is
    /// kept in `retired/`.
    pub fn retire(&self) -> Result<Retired, CaError> {
        let mut state = self.state.write().unwrap_or_else(PoisonError::into_inner);
        let (Some(rotation), Some(old)) = (
            state
                .rotation
                .clone()
                .filter(|r| r.stage == Stage::Switched),
            state.other.clone(),
        ) else {
            return Err(CaError::InvalidRequest(
                "the new certificate authority does not issue certificates yet".into(),
            ));
        };
        let retired = Retired {
            fingerprint: fingerprint_hex(&old.fingerprint),
            retired_at: now().unix_timestamp(),
            emergency: rotation.emergency,
            cert_pem: old.cert_pem.clone(),
        };
        let retired_dir = self.dir.join(RETIRED_DIR);
        create_dir(&retired_dir)?;
        let path = retired_dir.join(format!("{}.json", retired.fingerprint));
        let json = serde_json::to_vec_pretty(&retired).map_err(|e| CaError::corrupt(&path, e))?;
        store::write_atomic(&path, &json, 0o644)?;
        remove_file(&self.dir.join(ROTATION_FILE))?;
        remove_dir(&self.dir.join(PREVIOUS_DIR))?;
        state.other = None;
        state.rotation = None;
        state.retired.push(retired.clone());
        info!(fingerprint = %retired.fingerprint, "retired the old certificate authority");
        Ok(retired)
    }
}

fn rotation_emergency(state: &RotationState) -> bool {
    state.rotation.as_ref().is_some_and(|r| r.emergency)
}
