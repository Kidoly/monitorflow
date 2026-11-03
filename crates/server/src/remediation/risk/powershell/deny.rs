//! Built-in Windows deny rules (ADR-0084): what never runs, wherever it
//! hides in the command. Matched on the lenient [`fragments`] of the
//! command, lower case, with program names at any position, so a wrapper
//! such as `cmd /c` in front does not hide them.
//!
//! [`fragments`]: super::lexer::fragments

use super::rules::{fs_path, matches};

type Hit = Option<(&'static str, &'static str)>;

const FORMAT: (&str, &str) = (
    "forbidden.format",
    "formats, wipes or repartitions a disk, or writes to it directly",
);
const SYSTEM_FILES: (&str, &str) = (
    "forbidden.system_files",
    "deletes, or takes over, a drive root or Windows system files",
);
const DEFENDER: (&str, &str) = (
    "forbidden.defender",
    "turns off or weakens Microsoft Defender or the Windows firewall",
);
const ACCOUNTS: (&str, &str) = (
    "forbidden.accounts",
    "changes user accounts, groups or stored credentials, or copies secrets",
);
const RECOVERY: (&str, &str) = (
    "forbidden.recovery",
    "deletes backups or shadow copies, or changes boot and recovery settings",
);
const EVENT_LOGS: (&str, &str) = (
    "forbidden.event_logs",
    "clears or turns off event logs, which hold the evidence",
);
const ENCODED: (&str, &str) = (
    "forbidden.encoded",
    "hides the command it runs (encoded or base64), so it cannot be checked",
);

const REMOVERS: &[&str] = &[
    "remove-item",
    "rm",
    "del",
    "erase",
    "rd",
    "rmdir",
    "ri",
    "rm.exe",
    "del.exe",
];
const OWNERS: &[&str] = &[
    "icacls",
    "icacls.exe",
    "takeown",
    "takeown.exe",
    "cacls",
    "cacls.exe",
    "set-acl",
];
const FORMATTERS: &[&str] = &[
    "format-volume",
    "clear-disk",
    "initialize-disk",
    "remove-partition",
    "remove-physicaldisk",
    "reset-physicaldisk",
    "format",
    "format.com",
    "diskpart",
    "diskpart.exe",
];
const DEFENDER_CMDLETS: &[&str] = &[
    "set-mppreference",
    "add-mppreference",
    "remove-mppreference",
];
const DEFENDER_WORDS: &[&str] = &[
    "disableantispyware",
    "disableantivirus",
    "disablerealtimemonitoring",
];
/// Services of Defender and the firewall, and Defender's process.
const PROTECTION: &[&str] = &[
    "windefend",
    "wdnissvc",
    "sense",
    "wdfilter",
    "wdboot",
    "mdcoresvc",
    "securityhealthservice",
    "mpssvc",
    "bfe",
    "msmpeng",
    "msmpeng.exe",
];
/// Words that stop, pause, reconfigure or delete a service or process.
const STOPPERS: &[&str] = &[
    "stop-service",
    "spsv",
    "set-service",
    "suspend-service",
    "remove-service",
    "stop-process",
    "kill",
    "spps",
    "taskkill",
    "taskkill.exe",
    "stop",
    "pause",
    "config",
    "delete",
    "disabled",
];
const ACCOUNT_CMDLETS: &[&str] = &[
    "new-localuser",
    "set-localuser",
    "remove-localuser",
    "enable-localuser",
    "disable-localuser",
    "rename-localuser",
    "add-localgroupmember",
    "remove-localgroupmember",
    "new-aduser",
    "set-aduser",
    "remove-aduser",
    "enable-adaccount",
    "disable-adaccount",
    "unlock-adaccount",
    "set-adaccountpassword",
    "set-adaccountcontrol",
    "add-adgroupmember",
    "remove-adgroupmember",
    "add-adprincipalgroupmembership",
    "cmdkey",
    "cmdkey.exe",
    "ntdsutil",
    "ntdsutil.exe",
];
const HIVES: &[&str] = &[
    r"hklm\sam",
    r"hklm\security",
    r"hklm\system",
    r"hkey_local_machine\sam",
    r"hkey_local_machine\security",
    r"hkey_local_machine\system",
];
const EVENT_LOG_CMDLETS: &[&str] = &["clear-eventlog", "remove-eventlog", "limit-eventlog"];

/// Drive roots and Windows directories that are never deleted, as
/// components after the drive (the empty list is the root).
const CRITICAL: &[&[&str]] = &[
    &[],
    &["windows"],
    &["windows", "system32"],
    &["windows", "syswow64"],
    &["windows", "winsxs"],
    &["program files"],
    &["program files (x86)"],
    &["programdata"],
    &["users"],
    &["boot"],
];
/// Directories nothing inside of is deleted.
const PROTECTED: &[&[&str]] = &[
    &["windows", "system32"],
    &["windows", "syswow64"],
    &["windows", "winsxs"],
    &["windows", "boot"],
    &["boot"],
];

/// The built-in deny rule a lenient fragment matches, if any.
pub fn forbidden(words: &[String]) -> Hit {
    let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
    let after = |i: usize| &words[i + 1..];
    for (i, word) in words.iter().enumerate() {
        let w = word.as_str();
        if REMOVERS.contains(&w) && after(i).iter().any(|a| system_target(a)) {
            return Some(SYSTEM_FILES);
        }
        if OWNERS.contains(&w) && after(i).iter().any(|a| system_target(a)) {
            return Some(SYSTEM_FILES);
        }
        if FORMATTERS.contains(&w) || w.contains(r"\\.\physicaldrive") {
            return Some(FORMAT);
        }
        if DEFENDER_CMDLETS.contains(&w) || DEFENDER_WORDS.iter().any(|d| w.contains(d)) {
            return Some(DEFENDER);
        }
        if PROTECTION.contains(&w) && has(STOPPERS) {
            return Some(DEFENDER);
        }
        if w == "set-netfirewallprofile" && words.iter().any(|a| a.starts_with("-enabled")) {
            return Some(DEFENDER);
        }
        if ["netsh", "netsh.exe"].contains(&w)
            && (has(&["advfirewall", "firewall"]) && has(&["off", "disable"]))
        {
            return Some(DEFENDER);
        }
        if (w.ends_with("windowsfeature") || w.ends_with("windowsoptionalfeature"))
            && !w.starts_with("get-")
            && words.iter().any(|a| a.contains("defender"))
        {
            return Some(DEFENDER);
        }
        if ACCOUNT_CMDLETS.contains(&w) {
            return Some(ACCOUNTS);
        }
        if ["net", "net.exe", "net1", "net1.exe"].contains(&w)
            && after(i).first().is_some_and(|v| {
                ["user", "users", "localgroup", "group", "accounts"].contains(&v.as_str())
            })
            && after(i).len() > 1
        {
            return Some(ACCOUNTS);
        }
        if ["reg", "reg.exe"].contains(&w)
            && has(&["save", "export"])
            && words.iter().any(|a| HIVES.iter().any(|h| a.starts_with(h)))
        {
            return Some(ACCOUNTS);
        }
        if w.contains("lsass") && words.iter().any(|a| a.contains("dump")) {
            return Some(ACCOUNTS);
        }
        if ["vssadmin", "vssadmin.exe", "wbadmin", "wbadmin.exe"].contains(&w)
            && has(&["delete", "resize"])
        {
            return Some(RECOVERY);
        }
        if ["wmic", "wmic.exe"].contains(&w) && has(&["shadowcopy"]) && has(&["delete"]) {
            return Some(RECOVERY);
        }
        if ["bcdedit", "bcdedit.exe"].contains(&w)
            && words.iter().any(|a| {
                [
                    "/set",
                    "/delete",
                    "/deletevalue",
                    "-set",
                    "-delete",
                    "-deletevalue",
                ]
                .contains(&a.as_str())
            })
        {
            return Some(RECOVERY);
        }
        if ["reagentc", "reagentc.exe"].contains(&w) && has(&["/disable"]) {
            return Some(RECOVERY);
        }
        if w == "disable-computerrestore" {
            return Some(RECOVERY);
        }
        if EVENT_LOG_CMDLETS.contains(&w) {
            return Some(EVENT_LOGS);
        }
        if ["wevtutil", "wevtutil.exe"].contains(&w)
            && (has(&["cl", "clear-log"])
                || (has(&["sl", "set-log"])
                    && words
                        .iter()
                        .any(|a| a == "/e:false" || a == "/enabled:false")))
        {
            return Some(EVENT_LOGS);
        }
        if ["powershell", "powershell.exe", "pwsh", "pwsh.exe"].contains(&w)
            && after(i).iter().any(|a| encoded_parameter(a))
        {
            return Some(ENCODED);
        }
        if w.contains("frombase64string") {
            return Some(ENCODED);
        }
    }
    None
}

/// Built-in deny rules on the whole command, lower case: deleting shadow
/// copies through CIM or WMI, which spans a pipeline.
pub fn forbidden_text(command: &str) -> Hit {
    let text = command.to_ascii_lowercase();
    let removes = ["remove-ciminstance", "remove-wmiobject", ".delete("]
        .iter()
        .any(|r| text.contains(r));
    if text.contains("win32_shadowcopy") && removes {
        return Some(RECOVERY);
    }
    // `Get-Service WinDefend | Stop-Service`: the name and the stopper in
    // different stages.
    let words: Vec<&str> = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-')))
        .collect();
    let stops = words.iter().any(|w| PIPE_STOPPERS.contains(w));
    (stops && words.iter().any(|w| PROTECTION.contains(w))).then_some(DEFENDER)
}

/// Cmdlets that stop or reconfigure what a pipe gives them.
const PIPE_STOPPERS: &[&str] = &[
    "stop-service",
    "spsv",
    "set-service",
    "suspend-service",
    "remove-service",
    "stop-process",
    "kill",
    "spps",
];

/// `-EncodedCommand` of `powershell.exe` or one of its abbreviations.
fn encoded_parameter(word: &str) -> bool {
    let Some(name) = word.strip_prefix(['-', '/']) else {
        return false;
    };
    let name = name.split(':').next().unwrap_or_default();
    name == "ec" || (!name.is_empty() && "encodedcommand".starts_with(name))
}

/// Whether `word`, a comma-separated list of paths, names a drive root, a
/// critical directory, or something inside a protected one. A relative
/// path counts from the root of the system drive, where commands run.
fn system_target(word: &str) -> bool {
    let word = super::lexer::unprotect(word);
    word.split(',').any(|written| {
        if written.starts_with('-') {
            return false;
        }
        let trimmed = written.trim_end_matches(['\\', '/']);
        if trimmed.is_empty() {
            // `\`: the root of the current drive.
            return !written.is_empty();
        }
        // A short name (`PROGRA~1`) may stand for any directory. Switches
        // of `cmd.exe` (`/s`) become `\s`, which names nothing critical.
        let joined = trimmed
            .split(['\\', '/'])
            .map(|c| if c.contains('~') { "*" } else { c })
            .collect::<Vec<_>>()
            .join("\\");
        let path = match joined.as_bytes() {
            [c, b':', b'\\', ..] if c.is_ascii_alphabetic() => format!("{joined}\\"),
            [c, b':', ..] if c.is_ascii_alphabetic() => {
                format!("{}:\\{}\\", joined[..1].to_owned(), &joined[2..])
            }
            [b'\\', ..] => format!("c:{joined}"),
            _ => format!("c:\\{joined}"),
        };
        let Some(parts) = fs_path(&path) else {
            // `..` and other forms the rules cannot place.
            return path.contains("..");
        };
        let at = |dir: &[&str], exact: bool| {
            (if exact {
                parts.len() == dir.len()
            } else {
                parts.len() > dir.len()
            }) && parts.iter().zip(dir.iter()).all(|(p, d)| matches(p, d))
        };
        CRITICAL.iter().any(|dir| at(dir, true))
            || PROTECTED.iter().any(|dir| at(dir, false))
            || parts.iter().all(|p| p == "*")
    })
}
