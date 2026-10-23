//! Built-in rules (ADR-0041): the read-only allow-list of architecture §8,
//! low-impact changes, programs that are always high impact, read-only
//! filters for pipes, and built-in deny patterns.
//!
//! Every read-only rule lists the options it accepts; any other option,
//! such as `journalctl -f` or `tail -f` (never ends) or `dmesg -c`
//! (clears the buffer), makes the command unknown, hence high impact.

use super::lexer::Word;
use super::pattern::glob;
use super::{Decision, Rule, RuleSet, Tier};

/// The one rule whose file operands may hold unquoted globs, checked so
/// that no expansion leaves the allowed directories.
pub const GLOB_OPERANDS: Rule = Rule::Builtin {
    id: "readonly.log_file",
};

/// What the built-in rules say about a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Decided(Decision),
    /// No built-in rule matches; administrator allow rules may still.
    Unknown(String),
}

/// Privilege changes: their argument is another command.
const PRIVILEGE: &[&str] = &[
    "sudo", "su", "doas", "pkexec", "runuser", "sg", "newgrp", "setpriv", "capsh",
];

/// Shells and interpreters: they run code from their arguments.
const INTERPRETERS: &[&str] = &[
    "sh", "bash", "dash", "zsh", "ksh", "mksh", "fish", "csh", "tcsh", "busybox", "python",
    "python2", "python3", "perl", "ruby", "node", "nodejs", "php", "lua", "awk", "gawk", "mawk",
    "nawk", "tclsh", "expect", "jq",
];

/// Programs that run another command given in their arguments.
const WRAPPERS: &[&str] = &[
    "env",
    "nohup",
    "nice",
    "ionice",
    "timeout",
    "chroot",
    "nsenter",
    "unshare",
    "setsid",
    "stdbuf",
    "unbuffer",
    "xargs",
    "exec",
    "eval",
    "source",
    ".",
    "command",
    "builtin",
    "time",
    "watch",
    "flock",
    "chrt",
    "taskset",
    "systemd-run",
    "script",
    "find",
    "parallel",
    "strace",
    "ltrace",
    "gdb",
    "at",
    "batch",
    "trap",
];

/// Package managers.
const PACKAGES: &[&str] = &[
    "apt", "apt-get", "aptitude", "dpkg", "dnf", "yum", "microdnf", "rpm", "zypper", "snap",
    "flatpak", "pip", "pip3", "npm", "gem", "apk", "pacman",
];

/// Programs that change files, users, mounts, kernel or network settings.
const CHANGES: &[&str] = &[
    "rm",
    "rmdir",
    "mv",
    "cp",
    "install",
    "ln",
    "mkdir",
    "touch",
    "truncate",
    "dd",
    "shred",
    "sed",
    "tee",
    "vi",
    "vim",
    "nano",
    "ed",
    "emacs",
    "patch",
    "chmod",
    "chown",
    "chgrp",
    "chattr",
    "setfacl",
    "sysctl",
    "mount",
    "umount",
    "swapon",
    "swapoff",
    "useradd",
    "usermod",
    "userdel",
    "groupadd",
    "groupdel",
    "passwd",
    "chpasswd",
    "visudo",
    "crontab",
    "iptables",
    "ip6tables",
    "nft",
    "ufw",
    "firewall-cmd",
    "hostnamectl",
    "timedatectl",
    "modprobe",
    "rmmod",
    "insmod",
    "update-grub",
    "grub-install",
    "tar",
    "unzip",
    "gzip",
    "gunzip",
    "logrotate",
];

const KILL: &[&str] = &["kill", "pkill", "killall", "skill"];

const POWER: &[&str] = &["reboot", "shutdown", "poweroff", "halt", "init", "telinit"];

/// Whether an allow rule naming `program` could ever apply: the built-in
/// rules decide these programs themselves.
pub fn is_reserved(program: &str) -> bool {
    [
        PRIVILEGE,
        INTERPRETERS,
        WRAPPERS,
        PACKAGES,
        CHANGES,
        KILL,
        POWER,
    ]
    .iter()
    .any(|list| list.contains(&program))
        || program == "systemctl"
}

/// The verdict on `program` (already resolved) with `args`, the first or
/// only stage of a pipeline.
pub fn command(program: &str, args: &[Word], rules: &RuleSet) -> Verdict {
    let read_only =
        |id, reason: &str| Verdict::Decided(Decision::builtin(Tier::ReadOnly, id, reason));
    let high = |id, reason: String| Verdict::Decided(Decision::high(id, reason));
    let matched = match program {
        "df" => parse(args, &DF).is_some_and(|p| absolute(&p.operands)),
        "free" => parse(args, &FREE).is_some_and(|p| p.operands.is_empty()),
        "uptime" => parse(args, &UPTIME).is_some_and(|p| p.operands.is_empty()),
        "uname" => parse(args, &UNAME).is_some_and(|p| p.operands.is_empty()),
        "whoami" | "nproc" => args.is_empty(),
        "hostname" => match args {
            [] => true,
            [flag] => HOSTNAME_FLAGS.contains(&flag.text.as_str()),
            _ => false,
        },
        "id" => parse(args, &ID).is_some_and(|p| p.operands.len() <= 1),
        "date" => {
            args.len() <= 2
                && args
                    .iter()
                    .all(|a| DATE_FLAGS.contains(&a.text.as_str()) || a.text.starts_with('+'))
        }
        // These only report: no option of theirs changes anything.
        "ps" | "netstat" | "lscpu" | "who" | "w" => true,
        "ss" => args.iter().all(|a| ss_arg(&a.text)),
        "top" => parse(args, &TOP).is_some_and(|p| {
            p.operands.is_empty()
                && p.has("-b")
                && p.values("-n").count() > 0
                && p.values("-n").all(|v| v == "1")
        }),
        "vmstat" => parse(args, &VMSTAT).is_some_and(|p| match p.operands.as_slice() {
            [] => true,
            [delay, count] => small(&delay.text, 60) && small(&count.text, 100),
            _ => false,
        }),
        "lsblk" => parse(args, &LSBLK)
            .is_some_and(|p| p.operands.iter().all(|w| w.text.starts_with("/dev/"))),
        "ip" => ip_read(args),
        "dmesg" => parse(args, &DMESG).is_some_and(|p| p.operands.is_empty()),
        "ls" => parse(args, &LS).is_some_and(|p| absolute(&p.operands)),
        "journalctl" => journalctl(args),
        "cat" | "head" | "tail" | "grep" => {
            if file_reader(program, args) {
                return Verdict::Decided(Decision {
                    tier: Tier::ReadOnly,
                    rule: GLOB_OPERANDS,
                    reason: "reads files under /var/log or /proc".into(),
                });
            }
            false
        }
        "systemctl" => return systemctl(args),
        "find" => {
            if let Some(dir) = cache_clear(args, rules) {
                return Verdict::Decided(Decision {
                    tier: Tier::LowImpact,
                    rule: Rule::Configured { id: dir.id },
                    reason: format!("clears the configured cache directory {}", dir.path),
                });
            }
            false
        }
        "apt" | "apt-get" | "dnf" | "yum" if package_clean(program, args) => {
            return Verdict::Decided(Decision::builtin(
                Tier::LowImpact,
                "low.package_cache",
                "clears the package manager's cache",
            ))
        }
        _ => false,
    };
    let read_only_id = READ_ONLY_IDS.iter().find(|(p, _)| *p == program);
    if let (true, Some((_, id))) = (matched, read_only_id) {
        return read_only(id, &format!("`{program}` only reports system state"));
    }
    let reserved: [(&[&str], &'static str, &str); 7] = [
        (
            PRIVILEGE,
            "high.privilege",
            "changes privileges to run another command",
        ),
        (
            INTERPRETERS,
            "high.interpreter",
            "is a shell or interpreter that runs arbitrary code",
        ),
        (
            WRAPPERS,
            "high.wrapper",
            "runs another command given in its arguments",
        ),
        (PACKAGES, "high.package", "manages packages"),
        (
            CHANGES,
            "high.change",
            "changes files or system configuration",
        ),
        (KILL, "high.kill", "kills processes"),
        (POWER, "high.power", "reboots or shuts down the host"),
    ];
    for (list, id, what) in reserved {
        if list.contains(&program) {
            return high(id, format!("`{program}` {what}"));
        }
    }
    if read_only_id.is_some() {
        Verdict::Unknown(format!(
            "`{program}` with these arguments matches no read-only rule; unknown commands are high impact"
        ))
    } else {
        Verdict::Unknown(format!(
            "`{program}` is not a known command; unknown commands are high impact"
        ))
    }
}

/// Rule IDs of the read-only programs, for decisions and reasons.
const READ_ONLY_IDS: &[(&str, &str)] = &[
    ("df", "readonly.df"),
    ("free", "readonly.free"),
    ("uptime", "readonly.uptime"),
    ("uname", "readonly.uname"),
    ("whoami", "readonly.whoami"),
    ("nproc", "readonly.nproc"),
    ("hostname", "readonly.hostname"),
    ("id", "readonly.id"),
    ("date", "readonly.date"),
    ("ps", "readonly.ps"),
    ("netstat", "readonly.netstat"),
    ("lscpu", "readonly.lscpu"),
    ("who", "readonly.who"),
    ("w", "readonly.w"),
    ("ss", "readonly.ss"),
    ("top", "readonly.top"),
    ("vmstat", "readonly.vmstat"),
    ("lsblk", "readonly.lsblk"),
    ("ip", "readonly.ip"),
    ("dmesg", "readonly.dmesg"),
    ("ls", "readonly.ls"),
    ("journalctl", "readonly.journalctl"),
    ("cat", "readonly.log_file"),
    ("head", "readonly.log_file"),
    ("tail", "readonly.log_file"),
    ("grep", "readonly.log_file"),
];

/// Whether `program` with `args`, fed by a pipe, only filters its input.
pub fn filter(program: &str, args: &[Word]) -> bool {
    if args.iter().any(|a| a.glob) {
        return false;
    }
    let Some(parsed) = parse(
        args,
        match program {
            "grep" | "egrep" | "fgrep" => &GREP,
            "head" => &HEAD,
            "tail" => &TAIL,
            "sort" => &SORT,
            "uniq" => &UNIQ,
            "wc" => &WC,
            "cut" => &CUT,
            "tr" => &TR,
            _ => return false,
        },
    ) else {
        return false;
    };
    match program {
        "grep" | "egrep" | "fgrep" => {
            parsed.operands.len() == usize::from(!parsed.has("-e") && !parsed.has("--regexp"))
        }
        "head" | "tail" => parsed.operands.is_empty() && counts_ok(&parsed),
        "tr" => (1..=2).contains(&parsed.operands.len()),
        _ => parsed.operands.is_empty(),
    }
}

/// Command-line option grammar of one program.
#[derive(Debug, Default)]
struct Spec {
    /// Short options without value, as letters.
    short: &'static str,
    /// Short options taking a value, attached or in the next word.
    short_valued: &'static str,
    long: &'static [&'static str],
    /// Long options taking a value, after `=` or in the next word.
    long_valued: &'static [&'static str],
    /// `-<digits>` means `-n <digits>` (head, tail).
    numeric: bool,
}

#[derive(Debug, Default)]
struct Parsed<'a> {
    /// `-x` or `--name`, with the value if any.
    opts: Vec<(String, Option<&'a str>)>,
    operands: Vec<&'a Word>,
}

impl<'a> Parsed<'a> {
    fn has(&self, name: &str) -> bool {
        self.opts.iter().any(|(n, _)| n == name)
    }

    fn values<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'a str> + 's {
        self.opts
            .iter()
            .filter(move |(n, _)| n == name)
            .filter_map(|(_, v)| *v)
    }
}

/// Splits `args` into options and operands, `None` when an option is not
/// in `spec` or an option or value has an unquoted glob.
fn parse<'a>(args: &'a [Word], spec: &Spec) -> Option<Parsed<'a>> {
    let mut parsed = Parsed::default();
    let mut iter = args.iter();
    let mut only_operands = false;
    let next_value = |iter: &mut std::slice::Iter<'a, Word>| {
        iter.next().filter(|w| !w.glob).map(|w| w.text.as_str())
    };
    while let Some(word) = iter.next() {
        let text = word.text.as_str();
        if only_operands || text == "-" || !text.starts_with('-') {
            parsed.operands.push(word);
            continue;
        }
        if word.glob {
            return None;
        }
        if text == "--" {
            only_operands = true;
            continue;
        }
        if let Some(long) = text.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            if spec.long.contains(&name) && inline.is_none() {
                parsed.opts.push((format!("--{name}"), None));
            } else if spec.long_valued.contains(&name) {
                let value = match inline {
                    Some(value) => value,
                    None => next_value(&mut iter)?,
                };
                parsed.opts.push((format!("--{name}"), Some(value)));
            } else {
                return None;
            }
            continue;
        }
        let cluster = &text[1..];
        if spec.numeric && cluster.bytes().all(|b| b.is_ascii_digit()) {
            parsed.opts.push(("-n".into(), Some(cluster)));
            continue;
        }
        for (i, c) in cluster.char_indices() {
            if spec.short.contains(c) {
                parsed.opts.push((format!("-{c}"), None));
            } else if spec.short_valued.contains(c) {
                let rest = &cluster[i + c.len_utf8()..];
                let value = if rest.is_empty() {
                    next_value(&mut iter)?
                } else {
                    rest
                };
                parsed.opts.push((format!("-{c}"), Some(value)));
                break;
            } else {
                return None;
            }
        }
    }
    Some(parsed)
}

const DF: Spec = Spec {
    short: "hHTiklPa",
    long: &[
        "human-readable",
        "si",
        "inodes",
        "local",
        "print-type",
        "total",
        "portability",
        "all",
    ],
    ..EMPTY
};
const FREE: Spec = Spec {
    short: "hbkmgtlw",
    long: &[
        "human", "bytes", "kilo", "mega", "giga", "tera", "kibi", "mebi", "gibi", "tebi", "total",
        "wide", "lohi", "si",
    ],
    ..EMPTY
};
const UPTIME: Spec = Spec {
    short: "ps",
    long: &["pretty", "since"],
    ..EMPTY
};
const UNAME: Spec = Spec {
    short: "asnrvmpio",
    long: &[
        "all",
        "kernel-name",
        "nodename",
        "kernel-release",
        "kernel-version",
        "machine",
        "processor",
        "hardware-platform",
        "operating-system",
    ],
    ..EMPTY
};
const ID: Spec = Spec {
    short: "ugGnrz",
    ..EMPTY
};
const TOP: Spec = Spec {
    short: "bcHiS",
    short_valued: "no",
    ..EMPTY
};
const VMSTAT: Spec = Spec {
    short: "admDstwf",
    short_valued: "S",
    long: &[
        "active",
        "disk",
        "disk-sum",
        "slabs",
        "stats",
        "timestamp",
        "wide",
        "forks",
    ],
    long_valued: &["unit"],
    ..EMPTY
};
const LSBLK: Spec = Spec {
    short: "abdfJlmnOpPStz",
    short_valued: "o",
    long: &[
        "all",
        "bytes",
        "fs",
        "json",
        "list",
        "perms",
        "paths",
        "pairs",
        "topology",
        "noheadings",
    ],
    long_valued: &["output"],
    ..EMPTY
};
const DMESG: Spec = Spec {
    short: "Tkxtru",
    short_valued: "lf",
    long: &[
        "ctime",
        "kernel",
        "decode",
        "notime",
        "raw",
        "userspace",
        "reltime",
        "nopager",
    ],
    long_valued: &["level", "facility"],
    ..EMPTY
};
const LS: Spec = Spec {
    short: "aAlhtrSdiF1nR",
    long: &[
        "all",
        "almost-all",
        "human-readable",
        "inode",
        "directory",
        "classify",
        "numeric-uid-gid",
        "recursive",
        "full-time",
    ],
    ..EMPTY
};
const CAT: Spec = Spec {
    short: "AbeEnstTuv",
    long: &[
        "number",
        "number-nonblank",
        "show-all",
        "show-ends",
        "show-tabs",
        "squeeze-blank",
        "show-nonprinting",
    ],
    ..EMPTY
};
/// No `-f`, `-F` or `--follow`: they never end.
const TAIL: Spec = Spec {
    short: "qvz",
    short_valued: "nc",
    long: &["quiet", "silent", "verbose"],
    long_valued: &["lines", "bytes"],
    numeric: true,
};
const HEAD: Spec = TAIL;
/// No `-r`, `-R` or `-f FILE`: they read files outside the operands.
const GREP: Spec = Spec {
    short: "ivcnEFwxohHsqaI",
    short_valued: "emABC",
    long: &[
        "ignore-case",
        "invert-match",
        "count",
        "line-number",
        "extended-regexp",
        "fixed-strings",
        "word-regexp",
        "line-regexp",
        "only-matching",
        "no-filename",
        "with-filename",
        "no-messages",
        "quiet",
        "silent",
        "text",
    ],
    long_valued: &[
        "regexp",
        "max-count",
        "after-context",
        "before-context",
        "context",
    ],
    numeric: false,
};
/// No `-o`, `-T`, `--compress-program` or `--files0-from`.
const SORT: Spec = Spec {
    short: "bdfgihnrRuMVs",
    short_valued: "kt",
    long: &[
        "reverse",
        "numeric-sort",
        "human-numeric-sort",
        "general-numeric-sort",
        "month-sort",
        "version-sort",
        "unique",
        "ignore-case",
        "stable",
        "ignore-leading-blanks",
        "dictionary-order",
    ],
    long_valued: &["key", "field-separator"],
    numeric: false,
};
const UNIQ: Spec = Spec {
    short: "cdiuz",
    short_valued: "fsw",
    long: &["count", "repeated", "ignore-case", "unique"],
    ..EMPTY
};
const WC: Spec = Spec {
    short: "lcwmL",
    long: &["lines", "bytes", "words", "chars", "max-line-length"],
    ..EMPTY
};
const CUT: Spec = Spec {
    short: "s",
    short_valued: "dfcb",
    long: &["only-delimited"],
    long_valued: &["delimiter", "fields", "characters", "bytes"],
    ..EMPTY
};
const TR: Spec = Spec {
    short: "cCdst",
    ..EMPTY
};
const SYSTEMCTL: Spec = Spec {
    short: "laq",
    short_valued: "ntp",
    long: &[
        "no-pager",
        "full",
        "all",
        "plain",
        "no-legend",
        "value",
        "quiet",
        "failed",
        "no-block",
    ],
    long_valued: &["lines", "type", "state", "property"],
    ..EMPTY
};
/// No `-f`, `--vacuum-*`, `--rotate`, `--flush`, `--file`, `-D`, `--root`.
const JOURNALCTL: Spec = Spec {
    short: "kqrxameb",
    short_valued: "unSUpotg",
    long: &[
        "no-pager",
        "utc",
        "no-hostname",
        "reverse",
        "catalog",
        "quiet",
        "all",
        "dmesg",
        "merge",
        "no-full",
        "pager-end",
        "boot",
        "list-boots",
        "disk-usage",
    ],
    long_valued: &[
        "unit",
        "user-unit",
        "lines",
        "since",
        "until",
        "priority",
        "output",
        "identifier",
        "grep",
        "facility",
    ],
    numeric: false,
};
const EMPTY: Spec = Spec {
    short: "",
    short_valued: "",
    long: &[],
    long_valued: &[],
    numeric: false,
};

const HOSTNAME_FLAGS: &[&str] = &[
    "-f",
    "-s",
    "-d",
    "-i",
    "-I",
    "-A",
    "--fqdn",
    "--short",
    "--domain",
    "--ip-address",
    "--all-ip-addresses",
];
const DATE_FLAGS: &[&str] = &["-u", "--utc", "-R", "--rfc-email", "-I"];

fn absolute(operands: &[&Word]) -> bool {
    operands.iter().all(|w| w.text.starts_with('/'))
}

/// A decimal number from 0 to `max`.
fn small(text: &str, max: u32) -> bool {
    text.len() <= 6
        && text.bytes().all(|b| b.is_ascii_digit())
        && text.parse::<u32>().is_ok_and(|n| n <= max)
}

/// Line or byte counts of head and tail: digits with an optional sign.
fn counts_ok(parsed: &Parsed<'_>) -> bool {
    parsed.opts.iter().all(|(name, value)| match value {
        Some(v) if ["-n", "-c", "--lines", "--bytes"].contains(&name.as_str()) => {
            let digits = v.strip_prefix(['+', '-']).unwrap_or(v);
            (1..=9).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit())
        }
        _ => true,
    })
}

/// `ss` reports sockets, except `-K`/`--kill` (closes them), `-D`/`--diag`
/// (writes a file) and `-E`/`--events` (never ends).
fn ss_arg(arg: &str) -> bool {
    if let Some(long) = arg.strip_prefix("--") {
        !(long == "kill" || long == "events" || long.starts_with("diag"))
    } else if let Some(short) = arg.strip_prefix('-') {
        !short.contains(['K', 'D', 'E'])
    } else {
        true
    }
}

/// `ip [options] <object> [show|list|ls|get ...]` for objects that only
/// report.
fn ip_read(args: &[Word]) -> bool {
    const OPTIONS: &[&str] = &[
        "-4", "-6", "-br", "-brief", "-s", "-stats", "-d", "-details", "-c", "-color", "-j",
        "-json", "-p", "-pretty", "-o", "-oneline", "-h", "-human",
    ];
    const OBJECTS: &[&str] = &[
        "a",
        "addr",
        "address",
        "l",
        "link",
        "r",
        "route",
        "n",
        "neigh",
        "neighbor",
        "neighbour",
        "rule",
        "maddr",
        "maddress",
    ];
    const VERBS: &[&str] = &["show", "list", "ls", "lst"];
    let mut words = args
        .iter()
        .map(|w| w.text.as_str())
        .skip_while(|w| OPTIONS.contains(w));
    let object = match words.next() {
        Some(object) if OBJECTS.contains(&object) => object,
        _ => return false,
    };
    match words.next() {
        None => true,
        Some("get") => matches!(object, "r" | "route"),
        Some(verb) => VERBS.contains(&verb),
    }
}

/// `cat`, `head`, `tail` or `grep` of files under `/var/log` or `/proc`.
fn file_reader(program: &str, args: &[Word]) -> bool {
    let spec = match program {
        "cat" => &CAT,
        "head" => &HEAD,
        "tail" => &TAIL,
        _ => &GREP,
    };
    let Some(parsed) = parse(args, spec) else {
        return false;
    };
    let files = if program == "grep" && !parsed.has("-e") && !parsed.has("--regexp") {
        match parsed.operands.split_first() {
            Some((pattern, files)) if !pattern.glob => files,
            _ => return false,
        }
    } else {
        &parsed.operands[..]
    };
    !files.is_empty() && files.iter().all(|f| readable_path(f)) && counts_ok(&parsed)
}

/// `/proc` entries that expose secrets or lead elsewhere: environment and
/// memory, and symlinks to other roots, directories and files.
const PROC_DENIED: &[&str] = &[
    "environ",
    "mem",
    "kcore",
    "kmem",
    "pagemap",
    "root",
    "cwd",
    "exe",
    "fd",
    "map_files",
    "kpagecount",
    "kpageflags",
    "kpagecgroup",
    "keys",
    "sysrq-trigger",
];

/// A file under `/var/log` or `/proc`, without `.` or `..` components and
/// without `/proc` entries in [`PROC_DENIED`]. Globs are refused under
/// `/proc`, and under `/var/log` when a component could match `..` or
/// uses a bracket expression.
fn readable_path(word: &Word) -> bool {
    let path = word.text.as_str();
    let (rest, proc) = if let Some(rest) = path.strip_prefix("/var/log/") {
        (rest, false)
    } else if let Some(rest) = path.strip_prefix("/proc/") {
        (rest, true)
    } else {
        return false;
    };
    if proc && word.glob {
        return false;
    }
    rest.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !(word.glob && (part.contains('[') || glob(part.as_bytes(), b"..")))
            && !(proc && PROC_DENIED.contains(&part))
    })
}

/// `journalctl` with read-only options and `FIELD=value` matches.
fn journalctl(args: &[Word]) -> bool {
    // `-b` takes an optional boot offset or ID in the next word.
    let mut kept = Vec::with_capacity(args.len());
    let mut after_boot = false;
    for arg in args {
        if after_boot && boot_id(&arg.text) {
            after_boot = false;
            continue;
        }
        after_boot = arg.text == "-b" || arg.text == "--boot";
        kept.push(arg.clone());
    }
    let Some(parsed) = parse(&kept, &JOURNALCTL) else {
        return false;
    };
    let lines_ok = parsed
        .values("-n")
        .chain(parsed.values("--lines"))
        .all(|v| v == "all" || small(v, 1_000_000));
    lines_ok
        && parsed.operands.iter().all(|w| {
            w.text.split_once('=').is_some_and(|(field, _)| {
                !field.is_empty()
                    && field
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            })
        })
}

fn boot_id(text: &str) -> bool {
    let offset = text.strip_prefix(['-', '+']).unwrap_or(text);
    (!offset.is_empty() && offset.len() <= 4 && offset.bytes().all(|b| b.is_ascii_digit()))
        || (text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit()))
}

const SYSTEMCTL_READ: &[&str] = &[
    "status",
    "is-active",
    "is-enabled",
    "is-failed",
    "list-units",
    "list-unit-files",
    "list-timers",
    "list-sockets",
    "list-dependencies",
    "show",
    "cat",
];
const SYSTEMCTL_LOW: &[&str] = &[
    "start",
    "restart",
    "reload",
    "try-restart",
    "reload-or-restart",
    "try-reload-or-restart",
];
const SYSTEMCTL_POWER: &[&str] = &[
    "reboot",
    "poweroff",
    "halt",
    "kexec",
    "suspend",
    "hibernate",
    "hybrid-sleep",
    "rescue",
    "emergency",
    "isolate",
    "default",
    "soft-reboot",
];

/// Units whose restart can cut off the host or MonitorFlow itself.
const CRITICAL_UNITS: &[&str] = &[
    "ssh",
    "sshd",
    "dbus",
    "dbus-broker",
    "networking",
    "network",
    "NetworkManager",
    "polkit",
    "monitorflow-agent",
    "monitorflow-relay",
    "monitorflow-server",
];

/// Unit types that are not services one restarts as a routine fix.
const CRITICAL_SUFFIXES: &[&str] = &[
    ".target",
    ".mount",
    ".automount",
    ".slice",
    ".scope",
    ".device",
    ".swap",
];

fn systemctl(args: &[Word]) -> Verdict {
    let unknown = || {
        Verdict::Unknown(
            "`systemctl` with these arguments matches no rule; unknown commands are high impact"
                .into(),
        )
    };
    let parsed = parse(args, &SYSTEMCTL);
    let verb = match &parsed {
        Some(p) => p.operands.first().map(|w| w.text.as_str()),
        // Unknown options: the first operand-looking word is likely the verb.
        None => args
            .iter()
            .map(|w| w.text.as_str())
            .find(|w| !w.starts_with('-')),
    };
    let Some(verb) = verb else {
        return unknown();
    };
    if !SYSTEMCTL_READ.contains(&verb) && !SYSTEMCTL_LOW.contains(&verb) {
        return systemctl_change(verb);
    }
    let Some(parsed) = &parsed else {
        return unknown();
    };
    let units = &parsed.operands[1..];
    if SYSTEMCTL_READ.contains(&verb) {
        if parsed.has("--no-block") || !units.iter().all(|u| unit_name(&u.text, true)) {
            return unknown();
        }
        return Verdict::Decided(Decision::builtin(
            Tier::ReadOnly,
            "readonly.systemctl",
            format!("`systemctl {verb}` only reports unit state"),
        ));
    }
    if SYSTEMCTL_LOW.contains(&verb) {
        if units.is_empty() || !units.iter().all(|u| unit_name(&u.text, false)) {
            return unknown();
        }
        if let Some(unit) = units.iter().find(|u| critical_unit(&u.text)) {
            return Verdict::Decided(Decision::high(
                "high.critical_unit",
                format!(
                    "`{}` is critical to reach the host or to MonitorFlow",
                    unit.text
                ),
            ));
        }
        return Verdict::Decided(Decision::builtin(
            Tier::LowImpact,
            "low.systemctl",
            format!("`systemctl {verb}` of a service"),
        ));
    }
    systemctl_change(verb)
}

fn systemctl_change(verb: &str) -> Verdict {
    let (id, what) = if SYSTEMCTL_POWER.contains(&verb) {
        ("high.power", "changes the system state")
    } else {
        ("high.systemctl", "changes units or the service manager")
    };
    Verdict::Decided(Decision::high(id, format!("`systemctl {verb}` {what}")))
}

/// A systemd unit name; `patterns` also accepts `*?[]` (quoted, since
/// unquoted globs are refused earlier).
fn unit_name(name: &str, patterns: bool) -> bool {
    (1..=255).contains(&name.len())
        && !name.starts_with('-')
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || b":_.@-\\".contains(&b)
                || (patterns && b"*?[]".contains(&b))
        })
}

fn critical_unit(name: &str) -> bool {
    let base = name.strip_suffix(".service").unwrap_or(name);
    let base = base.split('@').next().unwrap_or(base);
    CRITICAL_UNITS.contains(&base)
        || base.starts_with("systemd-")
        || base.starts_with("getty")
        || CRITICAL_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// `find <dir> -mindepth 1 [-xdev] -delete` for a configured cache
/// directory, with or without a trailing slash.
fn cache_clear<'r>(args: &[Word], rules: &'r RuleSet) -> Option<&'r super::CacheDir> {
    let words: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
    let (dir, rest) = words.split_first()?;
    let shape = matches!(
        rest,
        ["-mindepth", "1", "-delete"]
            | ["-mindepth", "1", "-xdev", "-delete"]
            | ["-xdev", "-mindepth", "1", "-delete"]
    );
    if !shape {
        return None;
    }
    let dir = dir
        .strip_suffix('/')
        .filter(|d| !d.is_empty())
        .unwrap_or(dir);
    rules.cache_dirs.iter().find(|c| c.path == dir)
}

/// `apt[-get] clean|autoclean`, `dnf|yum clean <what>`.
fn package_clean(program: &str, args: &[Word]) -> bool {
    let words: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
    match program {
        "apt" | "apt-get" => matches!(words.as_slice(), ["clean"] | ["autoclean"]),
        _ => matches!(
            words.as_slice(),
            [
                "clean",
                "all" | "packages" | "metadata" | "dbcache" | "expire-cache"
            ]
        ),
    }
}

/// Directories whose recursive deletion is forbidden.
const SYSTEM_DIRS: &[&str] = &[
    "/", "/bin", "/boot", "/dev", "/etc", "/home", "/lib", "/lib32", "/lib64", "/opt", "/proc",
    "/root", "/sbin", "/srv", "/sys", "/usr", "/var",
];

/// The built-in deny rule a lenient fragment matches, if any. Program
/// names are matched at any position, so a wrapper in front does not hide
/// them.
pub fn forbidden(words: &[String]) -> Option<(&'static str, &'static str)> {
    for (i, word) in words.iter().enumerate() {
        let name = word.rsplit('/').next().unwrap_or(word);
        if name == "rm" && removes_system_dir(&words[i + 1..]) {
            return Some((
                "forbidden.rm_root",
                "recursively deletes / or a top-level system directory",
            ));
        }
        if name.starts_with("mkfs") || ["mke2fs", "mkswap", "wipefs", "blkdiscard"].contains(&name)
        {
            return Some(("forbidden.format", "formats or wipes a disk"));
        }
        if word.strip_prefix("of=").is_some_and(block_device)
            || (name == "shred" && words[i + 1..].iter().any(|w| block_device(w)))
        {
            return Some((
                "forbidden.raw_disk_write",
                "writes directly to a block device",
            ));
        }
    }
    None
}

/// Built-in deny rules that look at the raw text: a fork bomb, and output
/// redirected onto a block device.
pub fn forbidden_text(command: &str) -> Option<(&'static str, &'static str)> {
    let compact: String = command.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.contains(":(){") {
        return Some(("forbidden.fork_bomb", "is a fork bomb"));
    }
    let redirected = compact.match_indices('>').any(|(i, _)| {
        let target = compact[i + 1..].trim_start_matches(['>', '|', '&']);
        let target = target.trim_start_matches(['"', '\'']);
        block_device(target)
    });
    redirected.then_some((
        "forbidden.raw_disk_write",
        "writes directly to a block device",
    ))
}

fn removes_system_dir(args: &[String]) -> bool {
    let recursive = args.iter().any(|a| {
        a == "--recursive"
            || a == "--no-preserve-root"
            || (a.starts_with('-') && !a.starts_with("--") && a.contains(['r', 'R']))
    });
    recursive
        && args
            .iter()
            .filter(|a| a.starts_with('/'))
            .any(|a| SYSTEM_DIRS.contains(&lexical(a).as_str()))
}

/// `path` with empty, `.` and `*` components dropped and `..` applied.
fn lexical(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." | "*" => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// A whole-disk or partition device node.
fn block_device(path: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sd", "hd", "vd", "xvd", "nvme", "mmcblk", "dm-", "md", "mapper/", "disk/", "sr",
    ];
    path.strip_prefix("/dev/")
        .is_some_and(|dev| PREFIXES.iter().any(|p| dev.starts_with(p)))
}
