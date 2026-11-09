//! Built-in PowerShell rules (ADR-0084): read-only cmdlets and programs,
//! low-impact service and IIS restarts, commands that are always high
//! impact, and read-only filters for pipes.
//!
//! Names are matched without regard to case, as PowerShell does, after
//! resolving the built-in aliases these rules know. A program written
//! with `.exe` is the program, never an alias: `sc` is `Set-Content` in
//! Windows PowerShell 5.1, `sc.exe` the service control tool.
//!
//! Parameters are matched by their full name or a listed alias only.
//! PowerShell also accepts any unambiguous prefix, of a name or of an
//! alias, so a prefix the rules do not list makes the command unknown
//! rather than guessed at. No rule accepts every parameter but some.

use super::lexer::Word;
use crate::remediation::risk::pattern::glob;
use crate::remediation::risk::{Decision, Tier};

/// What the built-in rules say about a command.
pub use crate::remediation::risk::builtin::Verdict;

/// A command's name as the rules know it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// Lower case, aliases resolved, `.exe` removed.
    pub name: String,
    /// Written with `.exe`: a program file, never an alias or cmdlet.
    pub exe: bool,
}

/// Aliases of Windows PowerShell 5.1 that the read-only rules and the
/// filters use.
const ALIASES: &[(&str, &str)] = &[
    ("gsv", "get-service"),
    ("gps", "get-process"),
    ("ps", "get-process"),
    ("gci", "get-childitem"),
    ("ls", "get-childitem"),
    ("dir", "get-childitem"),
    ("gi", "get-item"),
    ("gc", "get-content"),
    ("cat", "get-content"),
    ("type", "get-content"),
    ("sls", "select-string"),
    ("gcim", "get-ciminstance"),
    ("gwmi", "get-wmiobject"),
    ("gdr", "get-psdrive"),
    ("select", "select-object"),
    ("sort", "sort-object"),
    ("where", "where-object"),
    ("ft", "format-table"),
    ("fl", "format-list"),
    ("fw", "format-wide"),
    ("measure", "measure-object"),
    ("group", "group-object"),
    ("sasv", "start-service"),
];

/// `word`, the first word of a stage, as the rules know it.
pub fn program(word: &str) -> Program {
    let lower = word.to_ascii_lowercase();
    if let Some(name) = lower.strip_suffix(".exe").filter(|n| !n.is_empty()) {
        return Program {
            name: name.to_owned(),
            exe: true,
        };
    }
    let name = ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower)
        .map_or(lower, |(_, name)| (*name).to_owned());
    Program { name, exe: false }
}

/// Whether `word` can name a command: letters, digits, `.`, `_` and `-`,
/// starting with a letter. Paths, quoted strings, numbers, `.` and `&`
/// cannot.
pub fn command_name(word: &Word) -> bool {
    !word.quoted
        && word.text.starts_with(|c: char| c.is_ascii_alphabetic())
        && word
            .text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Interpreters and programs that run code or other programs given in
/// their arguments.
const INTERPRETERS: &[&str] = &[
    "powershell",
    "powershell_ise",
    "pwsh",
    "cmd",
    "wscript",
    "cscript",
    "mshta",
    "rundll32",
    "regsvr32",
    "bash",
    "wsl",
    "python",
    "python3",
    "py",
    "perl",
    "node",
    "ruby",
    "php",
    "java",
    "msbuild",
    "installutil",
    "regasm",
    "regsvcs",
    "iex",
    "icm",
    "foreach-object",
    "foreach",
    "%",
    "start",
    "saps",
    "runas",
    "psexec",
    "psexec64",
    "sudo",
    "schtasks",
    "at",
    "forfiles",
    "wmic",
    "explorer",
    "msiexec",
    "ii",
    "conhost",
];

/// Programs and aliases that change files, services, accounts, the
/// network, disks or boot settings, or download files.
const CHANGES: &[&str] = &[
    "sc",
    "net",
    "net1",
    "netsh",
    "reg",
    "regedit",
    "icacls",
    "cacls",
    "takeown",
    "attrib",
    "bcdedit",
    "bootrec",
    "diskpart",
    "format",
    "chkdsk",
    "defrag",
    "sfc",
    "dism",
    "cipher",
    "compact",
    "fsutil",
    "vssadmin",
    "wbadmin",
    "certutil",
    "bitsadmin",
    "curl",
    "wget",
    "iwr",
    "irm",
    "del",
    "erase",
    "rd",
    "rmdir",
    "rm",
    "ri",
    "ren",
    "rni",
    "mv",
    "move",
    "mi",
    "cp",
    "copy",
    "cpi",
    "md",
    "mkdir",
    "ni",
    "set",
    "si",
    "sp",
    "clc",
    "clv",
    "xcopy",
    "robocopy",
    "mklink",
    "setx",
    "auditpol",
    "secedit",
    "gpupdate",
    "winrm",
    "wusa",
    "pnputil",
    "tee",
    "tee-object",
    "out-file",
    "out-printer",
    "wevtutil",
    "w32tm",
    "ipconfig",
    "cmdkey",
    "ntdsutil",
];

/// Processes and the host's power.
const KILL: &[&str] = &["kill", "spps", "taskkill", "tskill"];
const POWER: &[&str] = &["shutdown", "logoff", "restart-computer", "stop-computer"];

/// Cmdlet verbs that change something. A cmdlet with one of them is high
/// impact unless a rule below makes it low.
const CHANGE_VERBS: &[&str] = &[
    "add",
    "approve",
    "backup",
    "block",
    "checkpoint",
    "clear",
    "close",
    "connect",
    "copy",
    "debug",
    "deny",
    "disable",
    "disconnect",
    "dismount",
    "edit",
    "enable",
    "enter",
    "export",
    "grant",
    "import",
    "initialize",
    "install",
    "invoke",
    "limit",
    "lock",
    "mount",
    "move",
    "new",
    "optimize",
    "pop",
    "protect",
    "publish",
    "push",
    "register",
    "remove",
    "rename",
    "repair",
    "reset",
    "resize",
    "restart",
    "restore",
    "resume",
    "revoke",
    "save",
    "send",
    "set",
    "start",
    "stop",
    "submit",
    "suspend",
    "switch",
    "sync",
    "unblock",
    "uninstall",
    "unlock",
    "unprotect",
    "unregister",
    "update",
    "use",
    "wait",
    "write",
];

fn change_verb(name: &str) -> bool {
    name.split_once('-')
        .is_some_and(|(verb, _)| CHANGE_VERBS.contains(&verb))
}

fn read_only(id: &'static str, reason: impl Into<String>) -> Verdict {
    Verdict::Decided(Decision::builtin(Tier::ReadOnly, id, reason))
}

fn low(id: &'static str, reason: impl Into<String>) -> Verdict {
    Verdict::Decided(Decision::builtin(Tier::LowImpact, id, reason))
}

fn high(id: &'static str, reason: impl Into<String>) -> Verdict {
    Verdict::Decided(Decision::high(id, reason))
}

/// The verdict on `program` with `args`, the first or only stage of a
/// pipeline.
pub fn command(program: &Program, written: &str, args: &[Word]) -> Verdict {
    let name = program.name.as_str();
    if args.iter().any(|a| a.text.contains(r"\\")) {
        return Verdict::Unknown(format!(
            "`{written}` names a network path or another computer; unknown commands are high impact"
        ));
    }
    let decided = if program.exe {
        native(name, args)
    } else {
        cmdlet(name, args)
    };
    if let Some(verdict) = decided {
        return verdict;
    }
    if name == "sc" && !program.exe {
        return high(
            "high.change",
            "`sc` is Set-Content in Windows PowerShell, which writes files; `sc.exe` is the service tool",
        );
    }
    if !program.exe {
        if let Some(verdict) = native(name, args) {
            return verdict;
        }
    }
    let reserved: [(&[&str], &'static str, &str); 4] = [
        (
            INTERPRETERS,
            "high.interpreter",
            "runs code or another program given in its arguments",
        ),
        (
            CHANGES,
            "high.change",
            "changes files, services, accounts or system settings",
        ),
        (KILL, "high.kill", "kills processes"),
        (
            POWER,
            "high.power",
            "restarts, shuts down or logs off the host",
        ),
    ];
    for (list, id, what) in reserved {
        if list.contains(&name) {
            return high(id, format!("`{written}` {what}"));
        }
    }
    if change_verb(name) {
        return high(
            "high.change",
            format!("`{written}` changes the system (its verb is not a reading one)"),
        );
    }
    if known(name) {
        Verdict::Unknown(format!(
            "`{written}` with these arguments matches no read-only rule; unknown commands are high impact"
        ))
    } else {
        Verdict::Unknown(format!(
            "`{written}` is not a known command; unknown commands are high impact"
        ))
    }
}

/// Whether a rule above knows `name`, for the reason given when its
/// arguments did not fit.
fn known(name: &str) -> bool {
    generic_read(name).is_some()
        || NATIVE_READ.contains(&name)
        || [
            "get-childitem",
            "get-item",
            "get-content",
            "select-string",
            "restart-service",
            "start-service",
            "restart-webapppool",
            "start-webapppool",
            "start-website",
            "clear-dnsclientcache",
        ]
        .contains(&name)
}

/// Read-only cmdlets, each with the parameters it accepts by full name or
/// alias, in lower case and separated by spaces. They only select or
/// shape what the cmdlet reports about this computer. Any other parameter
/// makes the command unknown: those that reach another computer or
/// account (`-ComputerName` and its aliases, which differ by cmdlet:
/// `-Cn`, `-ServerName` on `Get-CimInstance`, `-IPAddress` on
/// `Get-HotFix`; `-CimSession`, `-Credential`, `-PolicyStore`,
/// `-GPOSession`, `-Cluster`), wait or never end (`-Continuous`,
/// `-AsJob`), read files (`-Path`, `-FilterXml`, `-Vhd`) or raise
/// privileges, and every prefix PowerShell would expand to one of them.
#[rustfmt::skip]
const GENERIC_READ: &[(&str, &str)] = &[
    (
        "get-service",
        "name servicename displayname include exclude dependentservices ds \
         requiredservices sdo servicesdependedon",
    ),
    ("get-process", "name processname id pid module fileversioninfo fv fvi includeusername"),
    ("get-hotfix", "id hfid description"),
    ("get-computerinfo", "property"),
    ("get-date", "format uformat displayhint date year month day hour minute second"),
    ("get-timezone", "id name listavailable"),
    ("get-culture", ""),
    ("get-volume", "driveletter filesystemlabel friendlyname objectid uniqueid"),
    ("get-disk", "number friendlyname serialnumber uniqueid"),
    ("get-physicaldisk", "friendlyname serialnumber uniqueid objectid usage healthstatus canpool"),
    ("get-partition", "disknumber partitionnumber number driveletter uniqueid"),
    ("get-psdrive", "name literalname psprovider scope"),
    ("get-storagepool", "friendlyname uniqueid isprimordial healthstatus"),
    ("get-virtualdisk", "friendlyname uniqueid healthstatus"),
    (
        "get-netipaddress",
        "ipaddress localaddress interfaceindex ifindex interfacealias ifalias \
         addressfamily type prefixlength prefixorigin suffixorigin addressstate \
         includeallcompartments",
    ),
    ("get-netipconfiguration", "interfacealias ifalias interfaceindex ifindex detailed all"),
    (
        "get-netadapter",
        "name ifalias interfacealias interfacedescription ifdesc interfaceindex ifindex \
         includehidden physical",
    ),
    (
        "get-netadapterstatistics",
        "name ifalias interfacealias interfacedescription ifdesc includehidden",
    ),
    (
        "get-nettcpconnection",
        "localaddress ipaddress localport remoteaddress remoteport state appliedsetting \
         owningprocess",
    ),
    ("get-netudpendpoint", "localaddress ipaddress localport owningprocess"),
    (
        "get-netroute",
        "destinationprefix interfaceindex ifindex interfacealias ifalias nexthop \
         addressfamily routemetric protocol state includeallcompartments",
    ),
    (
        "get-netneighbor",
        "ipaddress interfaceindex ifindex interfacealias ifalias linklayeraddress state \
         addressfamily includeallcompartments",
    ),
    (
        "get-netconnectionprofile",
        "name interfacealias interfaceindex networkcategory ipv4connectivity \
         ipv6connectivity",
    ),
    ("get-netfirewallprofile", "name profile all"),
    (
        "get-netfirewallrule",
        "name id displayname displaygroup group description enabled direction action \
         all status primarystatus policystoresource policystoresourcetype \
         edgetraversalpolicy owner",
    ),
    ("get-dnsclient", "interfaceindex interfacealias connectionspecificsuffix"),
    ("get-dnsclientcache", "entry name type status section data"),
    ("get-dnsclientserveraddress", "interfaceindex interfacealias addressfamily"),
    ("get-smbshare", "name scopename special includehidden"),
    ("get-smbsession", "sessionid clientcomputername clientusername scopename includehidden"),
    ("get-smbconnection", "servername username"),
    ("get-scheduledtask", "taskname taskpath"),
    ("get-scheduledtaskinfo", "taskname taskpath"),
    ("get-windowsfeature", "name"),
    ("get-mpcomputerstatus", ""),
    ("get-mpthreatdetection", "threatid"),
    ("get-iissite", "name"),
    ("get-iisapppool", "name"),
    ("get-website", "name"),
    ("get-webapppoolstate", "name"),
    ("get-webbinding", "name ipaddress port protocol hostheader"),
    ("get-clusternode", "name"),
    ("get-clustergroup", "name vmid"),
    ("get-clusterresource", "name vmid"),
    ("get-localuser", "name sid"),
    ("get-localgroup", "name sid"),
    ("get-localgroupmember", "name sid group member"),
    (
        "get-ciminstance",
        "classname namespace filter query querydialect property selectproperties \
         keyonly shallow operationtimeoutsec ot",
    ),
    (
        "get-wmiobject",
        "class classname namespace ns filter query property list recurse amended \
         directread locale",
    ),
    ("get-counter", "counter listset sampleinterval maxsamples"),
    (
        "get-eventlog",
        "logname ln newest after before username instanceid index entrytype et source \
         abo message msg asbaseobject list asstring",
    ),
    (
        "get-winevent",
        "logname listlog listprovider providername maxevents filterxpath force oldest",
    ),
];

/// Common parameters every generic read-only cmdlet also accepts.
const COMMON_PARAMETERS: &[&str] = &["erroraction", "ea", "warningaction", "wa", "verbose"];

/// The parameters `name` accepts, when it is a generic read-only cmdlet.
fn generic_read(name: &str) -> Option<&'static str> {
    GENERIC_READ
        .iter()
        .find(|(cmdlet, _)| *cmdlet == name)
        .map(|(_, parameters)| *parameters)
}

/// A cmdlet's verdict, or `None` when no rule decides it.
fn cmdlet(name: &str, args: &[Word]) -> Option<Verdict> {
    if let Some(accepted) = generic_read(name) {
        let ok = args.iter().all(|a| match a.parameter() {
            Some((p, _)) => {
                accepted.split(' ').any(|known| known == p)
                    || COMMON_PARAMETERS.contains(&p.as_str())
            }
            None => !a.dashed(),
        });
        return ok.then(|| {
            read_only(
                "readonly.cmdlet",
                format!("`{name}` only reports system state"),
            )
        });
    }
    match name {
        "get-childitem" => parse(args, &GET_CHILD_ITEM)
            .is_some_and(|p| p.paths(&[]).iter().all(|path| fs_path(path).is_some()))
            .then(|| read_only("readonly.list", "lists a directory")),
        "get-item" => parse(args, &GET_ITEM)
            .is_some_and(|p| {
                let paths = p.paths(&[]);
                !paths.is_empty() && paths.iter().all(|path| fs_path(path).is_some())
            })
            .then(|| read_only("readonly.list", "describes a file or directory")),
        "get-content" => {
            let parsed = parse(args, &GET_CONTENT)?;
            let paths = parsed.paths(&[]);
            let ok = !paths.is_empty()
                && paths.iter().all(|path| log_path(path))
                && parsed.numbers(&["tail", "totalcount", "head", "first", "last"]);
            ok.then(|| read_only("readonly.log_file", "reads log files"))
        }
        "select-string" => {
            let parsed = parse(args, &SELECT_STRING)?;
            let paths = parsed.paths(&["pattern"]);
            let ok = !paths.is_empty() && paths.iter().all(|path| log_path(path));
            ok.then(|| read_only("readonly.log_file", "searches log files"))
        }
        "restart-service" | "start-service" => Some(service_start(name, args)),
        "restart-webapppool" | "start-webapppool" | "start-website" => {
            let parsed = parse(args, &NAMED)?;
            let names = parsed.paths(&[]);
            (names.len() == 1 && names.iter().all(|n| !wildcard(n))).then(|| {
                low(
                    "low.iis",
                    format!("`{name}` starts or recycles one IIS site or application pool"),
                )
            })
        }
        "clear-dnsclientcache" => args
            .is_empty()
            .then(|| low("low.dns_cache", "clears the DNS client cache")),
        _ => None,
    }
}

/// Programs that only report, with the arguments they accept.
const NATIVE_READ: &[&str] = &[
    "hostname",
    "whoami",
    "ipconfig",
    "systeminfo",
    "tasklist",
    "netstat",
    "sc",
    "wevtutil",
    "w32tm",
    "driverquery",
];

/// A program's verdict, or `None` when no rule decides it. `sc` here is
/// `sc.exe`: the caller only asks for it when written so.
fn native(name: &str, args: &[Word]) -> Option<Verdict> {
    let lower: Vec<String> = args.iter().map(|a| a.text.to_ascii_lowercase()).collect();
    let lower: Vec<&str> = lower.iter().map(String::as_str).collect();
    let ok = match name {
        "hostname" => lower.is_empty(),
        "whoami" => flags(
            &lower,
            &[
                "/upn", "/fqdn", "/logonid", "/user", "/groups", "/claims", "/priv", "/all", "/nh",
            ],
            &["/fo"],
        ),
        "ipconfig" => match lower.as_slice() {
            []
            | ["/all"]
            | ["/displaydns"]
            | ["/allcompartments"]
            | ["/allcompartments", "/all"] => true,
            ["/flushdns"] => return Some(low("low.dns_cache", "clears the DNS client cache")),
            _ => false,
        },
        "systeminfo" => flags(&lower, &["/nh"], &["/fo"]),
        "tasklist" => flags(
            &lower,
            &["/svc", "/v", "/m", "/apps", "/nh"],
            &["/fo", "/fi"],
        ),
        "driverquery" => flags(&lower, &["/v", "/si", "/nh"], &["/fo"]),
        "netstat" => netstat(&lower),
        "sc" => matches!(
            lower.first(),
            Some(
                &("query"
                    | "queryex"
                    | "qc"
                    | "qdescription"
                    | "qfailure"
                    | "qprivs"
                    | "qsidtype"
                    | "qtriggerinfo"
                    | "getdisplayname"
                    | "getkeyname"
                    | "enumdepend")
            )
        ),
        "wevtutil" => wevtutil(&lower),
        "w32tm" => matches!(
            lower.as_slice(),
            [
                "/query",
                "/status" | "/source" | "/peers" | "/configuration" | "/tz"
            ] | [
                "/query",
                "/status" | "/peers" | "/configuration",
                "/verbose"
            ]
        ),
        _ => return None,
    };
    let id = match name {
        "sc" => "readonly.sc",
        "wevtutil" => "readonly.wevtutil",
        _ => "readonly.program",
    };
    ok.then(|| read_only(id, format!("`{name}` only reports system state")))
}

/// Whether `args` are only `switches` and `valued` options each followed
/// by a value.
fn flags(args: &[&str], switches: &[&str], valued: &[&str]) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if valued.contains(arg) {
            if args.next().is_none() {
                return false;
            }
        } else if !switches.contains(arg) {
            return false;
        }
    }
    true
}

/// `netstat` flags, without the interval that would repeat forever.
fn netstat(args: &[&str]) -> bool {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if *arg == "-p" {
            if !args
                .next()
                .is_some_and(|p| p.chars().all(|c| c.is_ascii_alphanumeric()))
            {
                return false;
            }
        } else if !arg
            .strip_prefix('-')
            .is_some_and(|f| !f.is_empty() && f.chars().all(|c| "abefnoqrstxy".contains(c)))
        {
            return false;
        }
    }
    true
}

/// `wevtutil` listing logs or querying events of this computer.
fn wevtutil(args: &[&str]) -> bool {
    const OPTIONS: &[&str] = &[
        "c",
        "count",
        "rd",
        "reversedirection",
        "f",
        "format",
        "q",
        "query",
        "e",
        "element",
    ];
    let Some((verb, rest)) = args.split_first() else {
        return false;
    };
    let positional = match *verb {
        "el" | "enum-logs" => 0,
        "gl" | "get-log" | "gli" | "get-log-info" | "qe" | "query-events" => 1,
        _ => return false,
    };
    let mut seen = 0;
    for arg in rest {
        match arg.strip_prefix('/') {
            Some(option) => {
                let name = option.split(':').next().unwrap_or_default();
                if !OPTIONS.contains(&name) {
                    return false;
                }
            }
            None => seen += 1,
        }
    }
    seen == positional
}

/// `Start-Service` or `Restart-Service` of named services, low impact
/// unless one is critical.
fn service_start(name: &str, args: &[Word]) -> Verdict {
    let Some(parsed) = parse(args, &NAMED) else {
        return Verdict::Unknown(format!(
            "`{name}` with these arguments matches no rule; unknown commands are high impact"
        ));
    };
    let names = parsed.paths(&[]);
    if names.is_empty() {
        return Verdict::Unknown(format!("`{name}` without a service name matches no rule"));
    }
    if names.iter().any(|n| wildcard(n)) {
        return high(
            "high.service_pattern",
            format!("`{name}` with a wildcard may act on many services"),
        );
    }
    if let Some(critical) = names
        .iter()
        .find(|n| CRITICAL_SERVICES.contains(&n.to_ascii_lowercase().as_str()))
    {
        return high(
            "high.critical_service",
            format!("`{critical}` keeps the host reachable, secure or monitored"),
        );
    }
    low(
        "low.service",
        format!("`{name}` starts or restarts the named services"),
    )
}

/// Services whose restart can cut off the host, its security or
/// MonitorFlow, by service name.
const CRITICAL_SERVICES: &[&str] = &[
    "rpcss",
    "rpceptmapper",
    "dcomlaunch",
    "lsm",
    "samss",
    "eventlog",
    "plugplay",
    "power",
    "profsvc",
    "gpsvc",
    "winmgmt",
    "eventsystem",
    "winrm",
    "termservice",
    "sessionenv",
    "umrdpservice",
    "lanmanserver",
    "lanmanworkstation",
    "netlogon",
    "dhcp",
    "dnscache",
    "nsi",
    "nlasvc",
    "bfe",
    "mpssvc",
    "windefend",
    "wdnissvc",
    "sense",
    "securityhealthservice",
    "keyiso",
    "vaultsvc",
    "kdc",
    "ntds",
    "dns",
    "trustedinstaller",
    "monitorflowagent",
    "monitorflowupdate",
];

fn wildcard(text: &str) -> bool {
    text.contains(['*', '?', '[', ']'])
}

/// Parameters a rule accepts: switches, parameters taking a value, the
/// order of positional ones, and which parameters hold paths or names.
struct Spec {
    switches: &'static [&'static str],
    valued: &'static [&'static str],
    /// Parameters positional arguments bind to, in order; `path` stands
    /// for `literalpath` too.
    positional: &'static [&'static str],
    /// Parameters whose values are paths (or names, for [`NAMED`]).
    paths: &'static [&'static str],
}

const GET_CHILD_ITEM: Spec = Spec {
    switches: &[
        "recurse",
        "force",
        "file",
        "directory",
        "name",
        "hidden",
        "system",
        "readonly",
    ],
    valued: &[
        "path",
        "literalpath",
        "filter",
        "include",
        "exclude",
        "depth",
    ],
    positional: &["path", "filter"],
    paths: &["path", "literalpath"],
};

const GET_ITEM: Spec = Spec {
    switches: &["force"],
    valued: &["path", "literalpath"],
    positional: &["path"],
    paths: &["path", "literalpath"],
};

const GET_CONTENT: Spec = Spec {
    switches: &["raw"],
    valued: &[
        "path",
        "literalpath",
        "tail",
        "totalcount",
        "head",
        "first",
        "last",
        "encoding",
    ],
    positional: &["path"],
    paths: &["path", "literalpath"],
};

const SELECT_STRING: Spec = Spec {
    switches: &[
        "simplematch",
        "casesensitive",
        "notmatch",
        "allmatches",
        "quiet",
        "list",
    ],
    valued: &["path", "literalpath", "pattern", "context", "encoding"],
    positional: &["pattern", "path"],
    paths: &["path", "literalpath"],
};

const NAMED: Spec = Spec {
    switches: &[],
    valued: &["name"],
    positional: &["name"],
    paths: &["name"],
};

/// Arguments that fit a [`Spec`].
struct Parsed<'a> {
    spec: &'a Spec,
    /// Lower-case parameter names with their values.
    params: Vec<(String, Option<&'a str>)>,
    positional: Vec<&'a str>,
}

/// `args` sorted by `spec`, or `None` when one does not fit.
fn parse<'a>(args: &'a [Word], spec: &'a Spec) -> Option<Parsed<'a>> {
    let mut parsed = Parsed {
        spec,
        params: Vec::new(),
        positional: Vec::new(),
    };
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.parameter() {
            Some((name, inline)) if spec.switches.contains(&name.as_str()) => {
                if inline.is_some() {
                    return None;
                }
                parsed.params.push((name, None));
            }
            Some((name, inline)) if spec.valued.contains(&name.as_str()) => {
                let value = match inline {
                    Some(value) if !value.is_empty() => value,
                    Some(_) => return None,
                    None => {
                        let next = args.next()?;
                        if next.parameter().is_some() {
                            return None;
                        }
                        next.text.as_str()
                    }
                };
                parsed.params.push((name, Some(value)));
            }
            Some(_) => return None,
            None if arg.dashed() => return None,
            None => parsed.positional.push(&arg.text),
        }
    }
    // A positional argument binds to the first positional parameter not
    // given by name; one left over makes PowerShell fail, and the rules
    // do not guess.
    let free = spec
        .positional
        .iter()
        .filter(|p| !parsed.named(p) && !(**p == "path" && parsed.named("literalpath")))
        .count();
    (parsed.positional.len() <= free).then_some(parsed)
}

impl<'a> Parsed<'a> {
    fn named(&self, name: &str) -> bool {
        self.params.iter().any(|(n, _)| n == name)
    }

    /// Values of the path (or name) parameters, each comma-separated list
    /// split. When none is named, the positional argument PowerShell binds
    /// to the path instead: the first one, or the next after the
    /// positional parameters `before` it that are not named.
    fn paths(&self, before: &[&str]) -> Vec<&'a str> {
        let named: Vec<&'a str> = self
            .params
            .iter()
            .filter(|(name, _)| self.spec.paths.contains(&name.as_str()))
            .filter_map(|(_, value)| *value)
            .collect();
        let values = if named.is_empty() {
            let position = before.iter().filter(|p| !self.named(p)).count();
            self.positional.get(position).copied().into_iter().collect()
        } else {
            named
        };
        values.iter().flat_map(|list| list.split(',')).collect()
    }

    /// Whether the values of `names` are small whole numbers.
    fn numbers(&self, names: &[&str]) -> bool {
        self.params
            .iter()
            .filter(|(name, _)| names.contains(&name.as_str()))
            .all(|(_, value)| {
                value.is_some_and(|v| v.len() <= 7 && v.chars().all(|c| c.is_ascii_digit()))
            })
    }
}

/// The lower-case components after the drive of `path`, an absolute
/// path on a local drive (`C:\...`, either slash), without `.` or `..`
/// components, alternate data streams, or a home directory; `None` for
/// anything else, provider paths such as `Env:` or `HKLM:` included.
pub fn fs_path(path: &str) -> Option<Vec<String>> {
    let path = path.replace('/', "\\").to_ascii_lowercase();
    let bytes = path.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
        return None;
    }
    let parts: Vec<String> = path[3..]
        .split('\\')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();
    let bad = |p: &String| {
        p == "." || p == ".." || p.contains([':', '~']) || p.trim_end_matches(['.', ' ']).is_empty()
    };
    (!parts.iter().any(bad)).then_some(parts)
}

/// Log directories, as components after the drive.
const LOG_DIRS: &[&[&str]] = &[
    &["windows", "logs"],
    &["windows", "system32", "logfiles"],
    &["inetpub", "logs"],
];

/// Whether `path` is a file under a log directory: the directory part
/// written out without wildcards, which may appear after it.
fn log_path(path: &str) -> bool {
    let Some(parts) = fs_path(path) else {
        return false;
    };
    LOG_DIRS
        .iter()
        .any(|dir| parts.len() > dir.len() && parts.iter().zip(dir.iter()).all(|(p, d)| p == d))
}

/// Whether `component` matches `name` as a PowerShell wildcard (`*`,
/// `?`); brackets are refused by the lexer outside quotes and treated as
/// matching anything here.
pub fn matches(component: &str, name: &str) -> bool {
    if component.contains(['[', ']']) {
        return true;
    }
    glob(component.as_bytes(), name.as_bytes())
}

/// Read-only filters fed by a pipe.
pub fn filter(program: &Program, args: &[Word]) -> bool {
    if program.exe {
        return false;
    }
    let spec = match program.name.as_str() {
        "select-object" => &SELECT_OBJECT,
        "sort-object" => &SORT_OBJECT,
        "where-object" => &WHERE_OBJECT,
        "format-table" => &FORMAT_TABLE,
        "format-list" | "format-wide" => &FORMAT_LIST,
        "out-string" => &OUT_STRING,
        "measure-object" => &MEASURE_OBJECT,
        "select-string" => &SELECT_STRING_FILTER,
        "group-object" => &GROUP_OBJECT,
        "convertto-json" => &CONVERTTO_JSON,
        "convertto-csv" => &CONVERTTO_CSV,
        _ => return false,
    };
    parse(args, spec).is_some()
}

const SELECT_OBJECT: Spec = Spec {
    switches: &["unique"],
    valued: &[
        "property",
        "expandproperty",
        "excludeproperty",
        "first",
        "last",
        "skip",
        "index",
    ],
    positional: &["property"],
    paths: &[],
};

const SORT_OBJECT: Spec = Spec {
    switches: &["descending", "unique", "casesensitive"],
    valued: &["property"],
    positional: &["property"],
    paths: &[],
};

/// The simplified syntax only: a property, an operator, a value.
const WHERE_OBJECT: Spec = Spec {
    switches: &[
        "eq",
        "ne",
        "gt",
        "ge",
        "lt",
        "le",
        "like",
        "notlike",
        "match",
        "notmatch",
        "contains",
        "notcontains",
        "in",
        "notin",
        "ceq",
        "cne",
        "clike",
        "cmatch",
        "not",
    ],
    valued: &["property", "value"],
    positional: &["property", "value"],
    paths: &[],
};

const FORMAT_TABLE: Spec = Spec {
    switches: &["autosize", "wrap", "hidetableheaders"],
    valued: &["property", "groupby"],
    positional: &["property"],
    paths: &[],
};

const FORMAT_LIST: Spec = Spec {
    switches: &[],
    valued: &["property", "groupby", "column"],
    positional: &["property"],
    paths: &[],
};

const OUT_STRING: Spec = Spec {
    switches: &["stream"],
    valued: &["width"],
    positional: &[],
    paths: &[],
};

const MEASURE_OBJECT: Spec = Spec {
    switches: &[
        "sum",
        "average",
        "maximum",
        "minimum",
        "line",
        "word",
        "character",
    ],
    valued: &["property"],
    positional: &["property"],
    paths: &[],
};

const SELECT_STRING_FILTER: Spec = Spec {
    switches: &[
        "simplematch",
        "casesensitive",
        "notmatch",
        "allmatches",
        "quiet",
    ],
    valued: &["pattern", "context"],
    positional: &["pattern"],
    paths: &[],
};

const GROUP_OBJECT: Spec = Spec {
    switches: &["noelement"],
    valued: &["property"],
    positional: &["property"],
    paths: &[],
};

const CONVERTTO_JSON: Spec = Spec {
    switches: &["compress"],
    valued: &["depth"],
    positional: &[],
    paths: &[],
};

const CONVERTTO_CSV: Spec = Spec {
    switches: &["notypeinformation"],
    valued: &["delimiter"],
    positional: &[],
    paths: &[],
};
