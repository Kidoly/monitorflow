//! The Windows Update Agent COM calls of the update service (ADR-0049),
//! made as LocalSystem. Every function blocks: the service runs them on
//! the blocking pool, one request at a time.
//!
//! Every COM method of the `windows` crate is `unsafe`, as it calls
//! through a vtable. The calls below are sound for the same reasons:
//! each interface pointer comes from `CoCreateInstance` or from another
//! interface's getter, which return owned, reference-counted pointers the
//! crate releases on drop, and COM is initialized on the calling thread
//! for as long as they live (see [`Com`]). Arguments are owned `BSTR`s
//! and interfaces that outlive the call.

use std::path::PathBuf;

use windows::core::{Interface, BSTR};
use windows::Win32::Foundation::{DECIMAL, VARIANT_BOOL, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::UpdateAgent::{
    irbNeverReboots, orcSucceeded, orcSucceededWithErrors, ISystemInformation, IUpdate, IUpdate2,
    IUpdateCollection, IUpdateInstaller2, IUpdateSession, SystemInformation, UpdateCollection,
    UpdateSession,
};

use super::{select, Found, InstallResult, Installed, Kind, Outcome, Reply, Request, Scan, Status};

/// Pending software updates that are not hidden, and installed ones.
const SCAN: &str =
    "IsInstalled=0 and IsHidden=0 and Type='Software' or IsInstalled=1 and Type='Software'";
/// What an install chooses from.
const PENDING: &str = "IsInstalled=0 and IsHidden=0 and Type='Software'";
/// How Windows Update's logs name the caller.
const CLIENT_ID: &str = "MonitorFlow";

type Result<T> = std::result::Result<T, String>;

/// `what` failed with `error`, with its HRESULT for searching.
fn fail(what: &'static str) -> impl Fn(windows::core::Error) -> String {
    move |error| {
        format!(
            "{what} failed ({:#010x}): {}",
            error.code().0,
            error.message()
        )
    }
}

fn yes(value: VARIANT_BOOL) -> bool {
    value.0 != 0
}

/// COM on this thread, multithreaded, until dropped. Interfaces must be
/// dropped first, so a `Com` is the first local of every entry point.
struct Com;

impl Com {
    fn init() -> Result<Self> {
        // SAFETY: no pointer argument; balanced by CoUninitialize in Drop,
        // also when COM was already initialized (S_FALSE).
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(fail("initializing COM"))?;
        Ok(Self)
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: matches the successful CoInitializeEx of `init`; the
        // interfaces of the entry point were declared after `self`, so
        // they are released already.
        unsafe { CoUninitialize() }
    }
}

/// Answers `request`, already checked.
pub fn handle(request: &Request) -> Reply {
    let result = match request {
        Request::Scan => scan().map(Reply::Updates),
        Request::Status => {
            status().map(|reboot_required| Reply::Status(Status { reboot_required }))
        }
        Request::Install { kind, updates } => install(*kind, updates).map(Reply::Installed),
        Request::Reboot => reboot().map(Reply::Rebooting),
        // The service answers it before calling this (ADR-0053).
        Request::SelfUpdate(_) => Err("self-updates are not Windows Update calls".into()),
    };
    result.unwrap_or_else(Reply::Error)
}

fn session() -> Result<IUpdateSession> {
    // SAFETY: see the module documentation.
    unsafe {
        let session: IUpdateSession = CoCreateInstance(&UpdateSession, None, CLSCTX_INPROC_SERVER)
            .map_err(fail("creating an update session"))?;
        session
            .SetClientApplicationID(&BSTR::from(CLIENT_ID))
            .map_err(fail("naming the update session"))?;
        Ok(session)
    }
}

/// The updates `criteria` selects, with what MonitorFlow reads of them.
fn search(session: &IUpdateSession, criteria: &str) -> Result<Vec<(IUpdate, Found)>> {
    // SAFETY: see the module documentation.
    unsafe {
        let searcher = session
            .CreateUpdateSearcher()
            .map_err(fail("creating an update searcher"))?;
        let result = searcher
            .Search(&BSTR::from(criteria))
            .map_err(fail("searching for updates"))?;
        let code = result
            .ResultCode()
            .map_err(fail("reading the search result"))?;
        if code != orcSucceeded && code != orcSucceededWithErrors {
            return Err(format!(
                "searching for updates ended with result code {}",
                code.0
            ));
        }
        let updates = result
            .Updates()
            .map_err(fail("reading the updates found"))?;
        let count = updates
            .Count()
            .map_err(fail("counting the updates found"))?;
        let mut found = Vec::with_capacity(count.max(0) as usize);
        for i in 0..count {
            let update = updates.get_Item(i).map_err(fail("reading an update"))?;
            let read = read(&update).map_err(fail("reading an update's details"))?;
            found.push((update, read));
        }
        Ok(found)
    }
}

/// What a report and a selection need of `update`.
fn read(update: &IUpdate) -> windows::core::Result<Found> {
    // SAFETY: see the module documentation.
    unsafe {
        let kbs = update.KBArticleIDs()?;
        let mut kb_ids = Vec::new();
        for i in 0..kbs.Count()? {
            kb_ids.push(kbs.get_Item(i)?.to_string());
        }
        let identity = update.Identity()?;
        let (mut classification_id, mut classification_name) = (String::new(), String::new());
        let categories = update.Categories()?;
        for i in 0..categories.Count()? {
            let category = categories.get_Item(i)?;
            if category.Type()? == "UpdateClassification" {
                classification_id = category.CategoryID()?.to_string();
                classification_name = category.Name()?.to_string();
                break;
            }
        }
        let installed = yes(update.IsInstalled()?);
        let reboot_required = if installed {
            yes(update.cast::<IUpdate2>()?.RebootRequired()?)
        } else {
            update.InstallationBehavior()?.RebootBehavior()? != irbNeverReboots
        };
        Ok(Found {
            title: update.Title()?.to_string(),
            kb_ids,
            update_id: identity.UpdateID()?.to_string(),
            revision: i64::from(identity.RevisionNumber()?),
            classification_id,
            classification_name,
            severity: update.MsrcSeverity()?.to_string(),
            size_bytes: bytes(update.MaxDownloadSize()?),
            installed,
            reboot_required,
        })
    }
}

/// A download size, which Windows Update gives as a `DECIMAL`; 0 when
/// negative or too large.
fn bytes(size: DECIMAL) -> u64 {
    // SAFETY: both union members of `DECIMAL` are plain integers over the
    // same bytes, so reading either is defined for any value.
    let (scale, sign, lo) = unsafe {
        (
            size.Anonymous1.Anonymous.scale,
            size.Anonymous1.Anonymous.sign,
            size.Anonymous2.Lo64,
        )
    };
    if sign != 0 || size.Hi32 != 0 || scale > 19 {
        return 0;
    }
    lo / 10u64.pow(u32::from(scale))
}

fn scan() -> Result<Scan> {
    let _com = Com::init()?;
    let session = session()?;
    let updates = search(&session, SCAN)?
        .iter()
        .map(|(_, found)| found.to_update())
        .collect();
    Ok(Scan {
        updates,
        reboot_required: status_with_com()?,
    })
}

fn status() -> Result<bool> {
    let _com = Com::init()?;
    status_with_com()
}

/// Whether Windows waits for a restart; COM is initialized.
fn status_with_com() -> Result<bool> {
    // SAFETY: see the module documentation.
    unsafe {
        let info: ISystemInformation =
            CoCreateInstance(&SystemInformation, None, CLSCTX_INPROC_SERVER)
                .map_err(fail("reading the system information"))?;
        Ok(yes(info
            .RebootRequired()
            .map_err(fail("reading the restart state"))?))
    }
}

fn collection() -> Result<IUpdateCollection> {
    // SAFETY: see the module documentation.
    unsafe {
        CoCreateInstance(&UpdateCollection, None, CLSCTX_INPROC_SERVER)
            .map_err(fail("creating an update collection"))
    }
}

/// Downloads and installs what `kind` and `names` select among the
/// pending updates.
fn install(kind: Kind, names: &[String]) -> Result<Installed> {
    let _com = Com::init()?;
    let session = session()?;
    let pending = search(&session, PENDING)?;
    let found: Vec<Found> = pending.iter().map(|(_, f)| f.clone()).collect();
    let (picked, not_pending) = select(kind, names, &found);
    tracing::info!(
        kind = kind.as_str(),
        selected = picked.len(),
        "installing updates"
    );
    let mut installed = Installed {
        not_pending,
        ..Installed::default()
    };
    if picked.is_empty() {
        installed.reboot_required = status_with_com()?;
        return Ok(installed);
    }
    // SAFETY: see the module documentation.
    unsafe {
        let selected = collection()?;
        for &i in &picked {
            let update = &pending[i].0;
            // Unattended installs must accept the terms, as Windows Update
            // for Business does.
            if !yes(update
                .EulaAccepted()
                .map_err(fail("reading a license state"))?)
            {
                update.AcceptEula().map_err(fail("accepting a license"))?;
            }
            selected.Add(update).map_err(fail("selecting an update"))?;
        }
        let downloader = session
            .CreateUpdateDownloader()
            .map_err(fail("creating an update downloader"))?;
        downloader
            .SetUpdates(&selected)
            .map_err(fail("selecting the downloads"))?;
        let download = downloader.Download().map_err(fail("downloading updates"))?;
        let ready = collection()?;
        let mut ready_index = Vec::new();
        for (position, &i) in picked.iter().enumerate() {
            let result = download
                .GetUpdateResult(position as i32)
                .map_err(fail("reading a download result"))?;
            let outcome = Outcome::from_code(
                result
                    .ResultCode()
                    .map_err(fail("reading a download result"))?
                    .0,
            );
            if outcome.succeeded() {
                ready
                    .Add(&pending[i].0)
                    .map_err(fail("selecting an update"))?;
                ready_index.push(i);
            } else {
                installed.results.push(InstallResult {
                    name: found[i].name(),
                    title: found[i].title.clone(),
                    result: outcome,
                    hresult: result.HResult().unwrap_or_default(),
                });
            }
        }
        let mut reboot_required = false;
        if !ready_index.is_empty() {
            let installer = session
                .CreateUpdateInstaller()
                .map_err(fail("creating an update installer"))?;
            installer
                .SetUpdates(&ready)
                .map_err(fail("selecting the installs"))?;
            installer
                .SetAllowSourcePrompts(VARIANT_FALSE)
                .map_err(fail("turning source prompts off"))?;
            if let Ok(quiet) = installer.cast::<IUpdateInstaller2>() {
                quiet
                    .SetForceQuiet(VARIANT_TRUE)
                    .map_err(fail("making the install quiet"))?;
            }
            let result = installer.Install().map_err(fail("installing updates"))?;
            reboot_required = yes(result
                .RebootRequired()
                .map_err(fail("reading the install"))?);
            for (position, &i) in ready_index.iter().enumerate() {
                let one = result
                    .GetUpdateResult(position as i32)
                    .map_err(fail("reading an install result"))?;
                installed.results.push(InstallResult {
                    name: found[i].name(),
                    title: found[i].title.clone(),
                    result: Outcome::from_code(
                        one.ResultCode()
                            .map_err(fail("reading an install result"))?
                            .0,
                    ),
                    hresult: one.HResult().unwrap_or_default(),
                });
            }
        }
        installed.reboot_required = reboot_required || status_with_com()?;
    }
    Ok(installed)
}

/// `%SystemRoot%\System32`: the service's environment is the system's.
fn system32() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    PathBuf::from(root).join("System32")
}

/// A planned restart for a hotfix in one minute, so the agent's result
/// and report get out first.
fn reboot() -> Result<()> {
    let shutdown = system32().join("shutdown.exe");
    let status = std::process::Command::new(&shutdown)
        .args([
            "/r",
            "/t",
            "60",
            "/d",
            "p:2:17",
            "/c",
            "MonitorFlow: updates installed",
        ])
        .status()
        .map_err(|e| format!("cannot run {}: {e}", shutdown.display()))?;
    if !status.success() {
        return Err(format!("shutdown.exe exited with {status}"));
    }
    tracing::warn!("restart in one minute requested by the agent");
    Ok(())
}
