//! Table-driven tests of [`classify`]: the built-in tiers, every syntax
//! rule, and bypass attempts.

use uuid::Uuid;

use super::lexer::{self, Word};
use super::*;

use Tier::{Forbidden as F, HighImpact as H, LowImpact as L, ReadOnly as R};

const ALLOW_RO: Uuid = Uuid::from_u128(1);
const ALLOW_LOW: Uuid = Uuid::from_u128(2);
const DENY: Uuid = Uuid::from_u128(3);
const CACHE: Uuid = Uuid::from_u128(4);
const ALLOW_LOW_TOO: Uuid = Uuid::from_u128(5);
const ALLOW_RO_TOO: Uuid = Uuid::from_u128(6);

fn rules() -> RuleSet {
    let p = |text| Pattern::parse(text).unwrap();
    RuleSet {
        allow: vec![
            AllowRule {
                id: ALLOW_RO,
                pattern: p("nginx -t"),
                tier: R,
            },
            AllowRule {
                id: ALLOW_LOW,
                pattern: p("/opt/app/bin/flush-queue **"),
                tier: L,
            },
            AllowRule {
                id: ALLOW_LOW_TOO,
                pattern: p("nginx -s *"),
                tier: L,
            },
            AllowRule {
                id: ALLOW_RO_TOO,
                pattern: p("/opt/app/bin/flush-queue"),
                tier: R,
            },
        ],
        deny: vec![DenyRule {
            id: DENY,
            pattern: p("systemctl stop postgresql*"),
        }],
        cache_dirs: vec![CacheDir {
            id: CACHE,
            path: "/var/cache/nginx".into(),
        }],
    }
}

fn check(cases: &[(&str, Tier, &str)], rules: &RuleSet) {
    let mut failures = Vec::new();
    for &(command, tier, rule) in cases {
        let decision = classify(command, rules);
        let id = match decision.rule {
            Rule::Builtin { id } => id.to_owned(),
            Rule::Configured { id } => format!("configured:{}", id.as_u128()),
        };
        if decision.tier != tier || id != rule {
            failures.push(format!(
                "{command:?}: got {} {id} ({}), want {tier} {rule}",
                decision.tier, decision.reason
            ));
        }
        assert!(!decision.reason.is_empty());
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn read_only_allow_list() {
    check(
        &[
            ("df -h", R, "readonly.df"),
            ("df -hT /var", R, "readonly.df"),
            ("df --inodes", R, "readonly.df"),
            ("free -m", R, "readonly.free"),
            ("free", R, "readonly.free"),
            ("uptime", R, "readonly.uptime"),
            ("uptime -p", R, "readonly.uptime"),
            ("ps aux", R, "readonly.ps"),
            ("ps -eo pid,comm,%mem --sort=-%mem", R, "readonly.ps"),
            ("top -bn1", R, "readonly.top"),
            ("top -b -n 1", R, "readonly.top"),
            ("top -b -n1 -o %MEM", R, "readonly.top"),
            ("ss -tlnp", R, "readonly.ss"),
            ("ss -s", R, "readonly.ss"),
            ("netstat -tlnp", R, "readonly.netstat"),
            ("uname -a", R, "readonly.uname"),
            ("hostname", R, "readonly.hostname"),
            ("hostname -f", R, "readonly.hostname"),
            ("whoami", R, "readonly.whoami"),
            ("id", R, "readonly.id"),
            ("date", R, "readonly.date"),
            ("date -u +%s", R, "readonly.date"),
            ("lsblk -f", R, "readonly.lsblk"),
            ("ip -br addr", R, "readonly.ip"),
            ("ip route show", R, "readonly.ip"),
            ("ip route get 1.1.1.1", R, "readonly.ip"),
            ("dmesg -T --level err", R, "readonly.dmesg"),
            ("vmstat", R, "readonly.vmstat"),
            ("vmstat 1 5", R, "readonly.vmstat"),
            ("ls -la /etc/nginx", R, "readonly.ls"),
            ("systemctl status nginx", R, "readonly.systemctl"),
            (
                "systemctl status nginx.service --no-pager -l",
                R,
                "readonly.systemctl",
            ),
            ("systemctl --failed", H, "default.unknown"),
            ("systemctl list-units --failed", R, "readonly.systemctl"),
            ("systemctl is-active 'php*-fpm'", R, "readonly.systemctl"),
            (
                "journalctl -u nginx -n 100 --no-pager",
                R,
                "readonly.journalctl",
            ),
            (
                "journalctl -u nginx --since '1 hour ago'",
                R,
                "readonly.journalctl",
            ),
            ("journalctl -p err -b", R, "readonly.journalctl"),
            ("journalctl -b -1 -k", R, "readonly.journalctl"),
            (
                "journalctl _SYSTEMD_UNIT=sshd.service",
                R,
                "readonly.journalctl",
            ),
            ("journalctl --disk-usage", R, "readonly.journalctl"),
            ("cat /var/log/syslog", R, "readonly.log_file"),
            ("cat /proc/meminfo", R, "readonly.log_file"),
            ("cat /proc/1/status", R, "readonly.log_file"),
            (
                "tail -n 200 /var/log/nginx/error.log",
                R,
                "readonly.log_file",
            ),
            ("tail -200 /var/log/nginx/error.log", R, "readonly.log_file"),
            ("head -c 4096 /var/log/messages", R, "readonly.log_file"),
            ("tail -n 50 /var/log/nginx/*.log", R, "readonly.log_file"),
            ("grep -i error /var/log/syslog", R, "readonly.log_file"),
            (
                "grep -c -e oom -e killed /var/log/kern.log",
                R,
                "readonly.log_file",
            ),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn pipes_into_read_only_filters() {
    check(
        &[
            ("ps aux | sort -rk 4 | head -n 10", R, "readonly.ps"),
            (
                "journalctl -u nginx -n 500 | grep -i error | wc -l",
                R,
                "readonly.journalctl",
            ),
            ("ss -tlnp | grep :443", R, "readonly.ss"),
            ("cat /var/log/syslog | tail -n 20", R, "readonly.log_file"),
            ("df -h | tr -s ' ' | cut -d ' ' -f 5", R, "readonly.df"),
            ("ps aux | uniq -c", R, "readonly.ps"),
            ("systemctl restart nginx | tail", L, "low.systemctl"),
            // Filters that read or write files, run code, or never end.
            ("ps aux | grep x /etc/shadow", H, "syntax.pipe_target"),
            ("ps aux | grep -r x /", H, "syntax.pipe_target"),
            ("ps aux | grep -f /etc/shadow", H, "syntax.pipe_target"),
            ("ps aux | sort -o /etc/passwd", H, "syntax.pipe_target"),
            (
                "ps aux | sort --compress-program=sh",
                H,
                "syntax.pipe_target",
            ),
            ("ps aux | uniq - /etc/passwd", H, "syntax.pipe_target"),
            ("ps aux | tail -f", H, "syntax.pipe_target"),
            ("ps aux | tee /tmp/x", H, "syntax.pipe_target"),
            ("ps aux | sh", H, "syntax.pipe_target"),
            ("ps aux | xargs kill", H, "syntax.pipe_target"),
            ("ps aux | awk '{print $2}'", H, "syntax.pipe_target"),
            ("ps aux | sed s/a/b/", H, "syntax.pipe_target"),
            ("ps aux | grep *", H, "syntax.pipe_target"),
            ("ps aux |", H, "syntax.pipe"),
            ("| ps aux", H, "syntax.pipe"),
            ("ps aux || true", H, "syntax.command_list"),
            ("ps aux |& grep x", H, "syntax.stderr_pipe"),
            ("echo hi | grep h", H, "default.unknown"),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn low_impact_changes() {
    check(
        &[
            ("systemctl restart nginx", L, "low.systemctl"),
            ("systemctl reload nginx.service", L, "low.systemctl"),
            ("systemctl try-restart php8.2-fpm", L, "low.systemctl"),
            ("systemctl restart nginx php-fpm", L, "low.systemctl"),
            ("systemctl start redis-server", L, "low.systemctl"),
            ("systemctl --no-block restart nginx", L, "low.systemctl"),
            ("apt-get clean", L, "low.package_cache"),
            ("dnf clean all", L, "low.package_cache"),
            (
                "find /var/cache/nginx -mindepth 1 -delete",
                L,
                "configured:4",
            ),
            (
                "find /var/cache/nginx/ -mindepth 1 -xdev -delete",
                L,
                "configured:4",
            ),
            // Not a configured directory, or another shape: find is opaque.
            ("find /var/cache/apt -mindepth 1 -delete", H, "high.wrapper"),
            ("find /var/cache/nginx -delete", H, "high.wrapper"),
            (
                "find /var/cache/nginx/.. -mindepth 1 -delete",
                H,
                "high.wrapper",
            ),
            (
                "find /var/cache/nginx -mindepth 1 -exec rm {} +",
                H,
                "syntax.grouping",
            ),
            ("rm -rf /var/cache/nginx/*", H, "syntax.glob"),
            // Critical units and other verbs.
            ("systemctl restart sshd", H, "high.critical_unit"),
            ("systemctl restart ssh.service", H, "high.critical_unit"),
            (
                "systemctl restart monitorflow-agent",
                H,
                "high.critical_unit",
            ),
            (
                "systemctl restart systemd-networkd",
                H,
                "high.critical_unit",
            ),
            ("systemctl restart getty@tty1", H, "high.critical_unit"),
            (
                "systemctl restart multi-user.target",
                H,
                "high.critical_unit",
            ),
            ("systemctl restart nginx sshd", H, "high.critical_unit"),
            ("systemctl restart 'ngin*'", H, "default.unknown"),
            ("systemctl restart", H, "default.unknown"),
            ("systemctl restart -H otherhost nginx", H, "default.unknown"),
            ("systemctl stop nginx", H, "high.systemctl"),
            ("systemctl disable --now nginx", H, "high.systemctl"),
            ("systemctl mask nginx", H, "high.systemctl"),
            ("systemctl daemon-reload", H, "high.systemctl"),
            ("systemctl reboot", H, "high.power"),
            ("systemctl isolate rescue.target", H, "high.power"),
        ],
        &rules(),
    );
}

#[test]
fn high_impact_by_category() {
    check(
        &[
            ("apt-get install -y nginx", H, "high.package"),
            ("apt upgrade", H, "high.package"),
            ("dnf remove httpd", H, "high.package"),
            ("dpkg -i x.deb", H, "high.package"),
            ("apt-get clean all", H, "high.package"),
            ("sed -i s/a/b/ /etc/nginx/nginx.conf", H, "high.change"),
            ("cp /tmp/x /etc/nginx/nginx.conf", H, "high.change"),
            ("chmod 777 /etc/shadow", H, "high.change"),
            ("rm /var/log/syslog.1", H, "high.change"),
            ("sysctl -w vm.swappiness=10", H, "high.change"),
            ("tee /etc/motd", H, "high.change"),
            ("kill -9 1234", H, "high.kill"),
            ("pkill nginx", H, "high.kill"),
            ("killall -HUP nginx", H, "high.kill"),
            ("reboot", H, "high.power"),
            ("shutdown -r now", H, "high.power"),
            ("init 0", H, "high.power"),
            ("sudo systemctl restart nginx", H, "high.privilege"),
            ("su -c 'df -h'", H, "high.privilege"),
            ("bash -c 'df -h'", H, "high.interpreter"),
            ("sh -c ls", H, "high.interpreter"),
            ("python3 -c 'print(1)'", H, "high.interpreter"),
            ("awk '{print}' /var/log/syslog", H, "high.interpreter"),
            ("env df -h", H, "high.wrapper"),
            ("timeout 5 df -h", H, "high.wrapper"),
            ("xargs rm", H, "high.wrapper"),
            ("nohup df", H, "high.wrapper"),
            ("watch df", H, "high.wrapper"),
            ("eval df -h", H, "high.wrapper"),
            ("exec df -h", H, "high.wrapper"),
            ("command df -h", H, "high.wrapper"),
            ("source /tmp/x", H, "high.wrapper"),
            (". /tmp/x", H, "high.wrapper"),
            ("curl -s http://localhost/health", H, "default.unknown"),
            ("nginx -s reload", H, "default.unknown"),
            ("frobnicate", H, "default.unknown"),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn read_only_programs_with_other_arguments_are_unknown() {
    check(
        &[
            ("journalctl -f", H, "default.unknown"),
            ("journalctl -u nginx --follow", H, "default.unknown"),
            ("journalctl --vacuum-size=100M", H, "default.unknown"),
            ("journalctl --rotate", H, "default.unknown"),
            ("journalctl --flush", H, "default.unknown"),
            ("journalctl -D /tmp/j", H, "default.unknown"),
            ("journalctl -n 10 notafield", H, "default.unknown"),
            ("tail -f /var/log/syslog", H, "default.unknown"),
            ("tail -F /var/log/syslog", H, "default.unknown"),
            ("tail --follow=name /var/log/syslog", H, "default.unknown"),
            ("dmesg -c", H, "default.unknown"),
            ("dmesg -C", H, "default.unknown"),
            ("dmesg -w", H, "default.unknown"),
            ("top", H, "default.unknown"),
            ("top -b", H, "default.unknown"),
            ("top -b -n 5", H, "default.unknown"),
            ("ss -K dst 1.2.3.4", H, "default.unknown"),
            ("ss --kill", H, "default.unknown"),
            ("ss -D /tmp/dump", H, "default.unknown"),
            ("ss -tE", H, "default.unknown"),
            ("hostname evil", H, "default.unknown"),
            ("date -s '2020-01-01'", H, "default.unknown"),
            ("date --set=2020-01-01", H, "default.unknown"),
            ("ip link set eth0 down", H, "default.unknown"),
            ("ip addr add 10.0.0.1/24 dev eth0", H, "default.unknown"),
            ("ip route flush all", H, "default.unknown"),
            ("ip link get eth0", H, "default.unknown"),
            ("free -s 1", H, "default.unknown"),
            ("vmstat 1", H, "default.unknown"),
            ("ls relative/path", H, "default.unknown"),
            ("df relative", H, "default.unknown"),
            ("lsblk /etc/passwd", H, "default.unknown"),
            ("whoami extra", H, "default.unknown"),
            ("systemctl status -H otherhost nginx", H, "default.unknown"),
            ("systemctl status --user nginx", H, "default.unknown"),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn file_readers_stay_under_var_log_and_safe_proc_entries() {
    check(
        &[
            ("cat /etc/shadow", H, "default.unknown"),
            ("cat /var/log/../../etc/shadow", H, "default.unknown"),
            ("cat /var/log/./syslog", H, "default.unknown"),
            ("cat /var/logs/x", H, "default.unknown"),
            ("cat /var/log", H, "default.unknown"),
            ("cat /var/log/", H, "default.unknown"),
            ("cat /var/log//syslog", H, "default.unknown"),
            ("cat var/log/syslog", H, "default.unknown"),
            ("cat", H, "default.unknown"),
            ("cat /var/log/syslog /etc/shadow", H, "default.unknown"),
            ("cat /proc/self/environ", H, "default.unknown"),
            ("cat /proc/1/environ", H, "default.unknown"),
            ("cat /proc/self/root/etc/shadow", H, "default.unknown"),
            ("cat /proc/1/cwd/secret", H, "default.unknown"),
            ("cat /proc/1/fd/3", H, "default.unknown"),
            ("cat /proc/kcore", H, "default.unknown"),
            ("cat /proc/1/task/1/root/etc/shadow", H, "default.unknown"),
            ("cat /proc/*/status", H, "syntax.glob"),
            ("cat /var/log/.*/x", H, "syntax.glob"),
            ("cat /var/log/.?/.?/etc/shadow", H, "syntax.glob"),
            ("cat /var/log/[s]yslog", H, "syntax.glob"),
            ("cat /etc/*", H, "syntax.glob"),
            ("tail -n /etc/shadow /var/log/syslog", H, "default.unknown"),
            ("tail -n 10 -- /var/log/syslog", R, "readonly.log_file"),
            ("grep root /etc/shadow", H, "default.unknown"),
            ("grep -r x /var/log", H, "default.unknown"),
            ("grep -f /etc/shadow /var/log/syslog", H, "default.unknown"),
            ("grep -e x", H, "default.unknown"),
            ("grep x* /var/log/syslog", H, "syntax.glob"),
            ("grep 'x*' /var/log/syslog", R, "readonly.log_file"),
            ("head -n -5 /var/log/syslog", R, "readonly.log_file"),
            ("cat -- -n", H, "default.unknown"),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn syntax_rules_raise_to_high_impact() {
    check(
        &[
            ("", H, "syntax.empty"),
            ("  \n\t ", H, "syntax.empty"),
            ("df -h; rm -rf /tmp/x", H, "syntax.command_list"),
            ("df -h && reboot", H, "syntax.command_list"),
            ("df -h || reboot", H, "syntax.command_list"),
            ("df -h & reboot", H, "syntax.command_list"),
            ("df -h\nreboot", H, "syntax.command_list"),
            ("df -h\r\nreboot", H, "syntax.control"),
            ("df $(echo -h)", H, "syntax.substitution"),
            ("df `echo -h`", H, "syntax.substitution"),
            ("df \"$(reboot)\"", H, "syntax.substitution"),
            ("cat $HOME/x", H, "syntax.substitution"),
            ("cat ${X}", H, "syntax.substitution"),
            ("df -h > /tmp/out", H, "syntax.redirection"),
            ("df -h >> /etc/motd", H, "syntax.redirection"),
            ("cat < /etc/shadow", H, "syntax.redirection"),
            ("df -h 2>&1", H, "syntax.redirection"),
            ("cat <<EOF", H, "syntax.redirection"),
            ("cat <<< x", H, "syntax.redirection"),
            ("(df -h)", H, "syntax.grouping"),
            ("{ df -h; }", H, "syntax.grouping"),
            ("cat /var/log/{syslog,auth.log}", H, "syntax.grouping"),
            ("df -h # comment", H, "syntax.comment"),
            ("! df -h", H, "syntax.history"),
            ("cat ~/x", H, "syntax.tilde"),
            ("df 'unterminated", H, "syntax.unterminated"),
            ("df \"unterminated", H, "syntax.unterminated"),
            ("df \\", H, "syntax.unterminated"),
            ("df \\\n-h", H, "syntax.continuation"),
            ("LANG=C df -h", H, "syntax.env_prefix"),
            ("PATH=/tmp df", H, "syntax.env_prefix"),
            ("ps aux | LC_ALL=C sort", H, "syntax.env_prefix"),
            ("df\x07 -h", H, "syntax.control"),
            ("df\0 -h", H, "syntax.control"),
            (&"df -h ".repeat(1000), H, "syntax.too_long"),
            ("ls -la /etc/*", H, "syntax.glob"),
            ("ps a?x", H, "syntax.glob"),
            ("chmod 644 /etc/*.conf", H, "syntax.glob"),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn quoting_and_path_tricks_do_not_bypass_rules() {
    check(
        &[
            // Leading and trailing whitespace and newlines are ignored.
            ("  df -h  ", R, "readonly.df"),
            ("\n\ndf -h\n", R, "readonly.df"),
            ("\tdf -h", R, "readonly.df"),
            // Quote removal happens before matching, as in the shell.
            ("'df' -h", R, "readonly.df"),
            ("d\\f -h", R, "readonly.df"),
            ("\"df\" \"-h\"", R, "readonly.df"),
            ("'r'm -rf /tmp/x", H, "high.change"),
            ("r\\m -rf /tmp/x", H, "high.change"),
            ("'sudo' df", H, "high.privilege"),
            ("\"ba\"sh -c 'df'", H, "high.interpreter"),
            ("'df -h'", H, "default.unknown"),
            ("'df -h; reboot'", H, "default.unknown"),
            // Escaped or quoted metacharacters are literal words.
            ("grep 'a;b' /var/log/syslog", R, "readonly.log_file"),
            ("grep 'a|b' /var/log/syslog", R, "readonly.log_file"),
            ("grep \\; /var/log/syslog", R, "readonly.log_file"),
            ("grep '$(reboot)' /var/log/syslog", R, "readonly.log_file"),
            ("grep \"\\$x\" /var/log/syslog", R, "readonly.log_file"),
            // Paths: standard directories resolve, others are unknown.
            ("/bin/df -h", R, "readonly.df"),
            ("/usr/bin/systemctl restart nginx", L, "low.systemctl"),
            ("/bin/rm -rf /tmp/x", H, "high.change"),
            ("/usr/bin/sudo df", H, "high.privilege"),
            ("/tmp/df -h", H, "default.unknown"),
            ("./df -h", H, "default.unknown"),
            ("/bin/../tmp/df -h", H, "default.unknown"),
            ("/usr/bin/", H, "default.unknown"),
            // Unicode lookalikes and invisible characters.
            ("ԁf -h", H, "syntax.non_ascii"),
            ("df −h", H, "syntax.non_ascii"),
            ("df\u{200b} -h", H, "syntax.non_ascii"),
            ("df\u{a0}-h", H, "syntax.non_ascii"),
            ("systemctl restart nginx\u{202e}", H, "syntax.non_ascii"),
            ("ｄｆ -h", H, "syntax.non_ascii"),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn built_in_deny_patterns_are_forbidden_wherever_they_hide() {
    check(
        &[
            ("rm -rf /", F, "forbidden.rm_root"),
            ("rm -fr /*", F, "forbidden.rm_root"),
            ("rm -r -f /etc", F, "forbidden.rm_root"),
            ("rm --recursive --force /usr/", F, "forbidden.rm_root"),
            ("rm -rf --no-preserve-root /", F, "forbidden.rm_root"),
            ("rm -rf //", F, "forbidden.rm_root"),
            ("rm -rf /var/../", F, "forbidden.rm_root"),
            ("/bin/rm -rf /", F, "forbidden.rm_root"),
            ("sudo rm -rf /", F, "forbidden.rm_root"),
            ("df -h; rm -rf /", F, "forbidden.rm_root"),
            ("df -h && rm -rf /", F, "forbidden.rm_root"),
            ("df -h\nrm -rf /", F, "forbidden.rm_root"),
            ("df $(rm -rf /)", F, "forbidden.rm_root"),
            ("df `rm -rf /`", F, "forbidden.rm_root"),
            ("echo \"$(rm -rf /)\"", F, "forbidden.rm_root"),
            ("bash -c 'rm -rf /'", F, "forbidden.rm_root"),
            ("sh -c \"sh -c 'rm -rf /'\"", F, "forbidden.rm_root"),
            ("'rm' '-rf' '/'", F, "forbidden.rm_root"),
            ("r\\m -rf /", F, "forbidden.rm_root"),
            ("ps aux | xargs rm -rf /", F, "forbidden.rm_root"),
            ("rm -rf /var/cache/nginx", H, "high.change"),
            ("mkfs.ext4 /dev/sda1", F, "forbidden.format"),
            ("/sbin/mkfs -t xfs /dev/vdb", F, "forbidden.format"),
            ("wipefs -a /dev/sdb", F, "forbidden.format"),
            ("mkswap /dev/sdb2", F, "forbidden.format"),
            (
                "dd if=/dev/zero of=/dev/sda bs=1M",
                F,
                "forbidden.raw_disk_write",
            ),
            ("dd if=/dev/zero of=/tmp/file", H, "high.change"),
            ("shred -n 1 /dev/nvme0n1", F, "forbidden.raw_disk_write"),
            ("echo x > /dev/sda", F, "forbidden.raw_disk_write"),
            (
                "cat /tmp/img >>/dev/mapper/root",
                F,
                "forbidden.raw_disk_write",
            ),
            (":(){ :|:& };:", F, "forbidden.fork_bomb"),
            // Deny wins over a malformed command and over non-ASCII text.
            ("rm -rf / 'unterminated", F, "forbidden.rm_root"),
            ("ԁf; rm -rf /", F, "forbidden.rm_root"),
        ],
        &RuleSet::default(),
    );
}

#[test]
fn administrator_rules() {
    check(
        &[
            // Allow rules apply to commands no built-in rule knows.
            ("nginx -t", R, "configured:1"),
            ("nginx -s reload", L, "configured:5"),
            ("nginx -T", H, "default.unknown"),
            // The most cautious matching allow rule wins.
            ("/opt/app/bin/flush-queue --all", L, "configured:2"),
            ("/opt/app/bin/flush-queue", L, "configured:2"),
            ("flush-queue --all", H, "default.unknown"),
            (
                "/opt/app/bin/flush-queue x; reboot",
                H,
                "syntax.command_list",
            ),
            (
                "/opt/app/bin/flush-queue $(reboot)",
                H,
                "syntax.substitution",
            ),
            (
                "/opt/app/bin/flush-queue x > /etc/passwd",
                H,
                "syntax.redirection",
            ),
            ("/opt/app/bin/flush-queue *", H, "syntax.glob"),
            ("X=1 nginx -t", H, "syntax.env_prefix"),
            ("nginx -t | sh", H, "syntax.pipe_target"),
            ("nginx -t | grep ok", R, "configured:1"),
            // Deny patterns win over everything, built-ins included.
            ("systemctl stop postgresql", F, "configured:3"),
            ("systemctl stop postgresql@15-main", F, "configured:3"),
            ("/usr/bin/systemctl stop postgresql", F, "configured:3"),
            ("sudo systemctl  stop   postgresql", F, "configured:3"),
            ("df -h; systemctl stop postgresql", F, "configured:3"),
            ("bash -c 'systemctl stop postgresql'", F, "configured:3"),
            ("systemctl stop nginx", H, "high.systemctl"),
            // Cache directories.
            (
                "find /var/cache/nginx -mindepth 1 -delete",
                L,
                "configured:4",
            ),
        ],
        &rules(),
    );
}

#[test]
fn allow_rules_never_lower_built_in_decisions() {
    let p = |text| Pattern::parse(text).unwrap();
    let permissive = RuleSet {
        allow: [
            "**",
            "sudo **",
            "bash **",
            "systemctl **",
            "kill **",
            "cat **",
            "find **",
        ]
        .into_iter()
        .map(|text| AllowRule {
            id: Uuid::from_u128(9),
            pattern: p(text),
            tier: R,
        })
        .collect(),
        ..RuleSet::default()
    };
    check(
        &[
            ("sudo reboot", H, "high.privilege"),
            ("bash -c id", H, "high.interpreter"),
            ("systemctl stop nginx", H, "high.systemctl"),
            ("systemctl restart nginx", L, "low.systemctl"),
            ("kill 1", H, "high.kill"),
            ("find / -delete", H, "high.wrapper"),
            ("cat /var/log/syslog;id", H, "syntax.command_list"),
            ("cat /etc/*", H, "syntax.glob"),
            // Read-only programs with other arguments are unknown, so an
            // administrator may allow them.
            ("cat /etc/hosts", R, "configured:9"),
            ("frobnicate", R, "configured:9"),
        ],
        &permissive,
    );
}

#[test]
fn decisions_serialize_for_the_api() {
    let decision = classify("df -h", &RuleSet::default());
    assert_eq!(
        serde_json::to_value(&decision).unwrap(),
        serde_json::json!({
            "tier": "read_only",
            "rule": {"source": "builtin", "id": "readonly.df"},
            "reason": "`df` only reports system state"
        })
    );
    let decision = classify("nginx -t", &rules());
    assert_eq!(
        serde_json::to_value(decision.rule).unwrap(),
        serde_json::json!({"source": "configured", "id": ALLOW_RO})
    );
    for tier in [R, L, H, F] {
        assert_eq!(Tier::parse(tier.as_str()), Some(tier));
        assert_eq!(serde_json::to_value(tier).unwrap(), tier.as_str());
    }
    assert!(R < L && L < H && H < F);
}

#[test]
fn the_lexer_splits_like_a_shell() {
    let words = |stage: &[Word]| stage.iter().map(|w| w.text.clone()).collect::<Vec<_>>();
    let stages = lexer::lex("grep -e 'a b' \"c\\\"d\" e\\ f | wc -l").unwrap();
    assert_eq!(words(&stages[0]), ["grep", "-e", "a b", "c\"d", "e f"]);
    assert_eq!(words(&stages[1]), ["wc", "-l"]);
    assert_eq!(stages[0][0], Word::plain("grep"));
    assert!(stages[0][2].quoted && !stages[0][2].glob);
    let stages = lexer::lex("ls /a* '/b*' /c\\? \"\\x\"").unwrap();
    assert_eq!(words(&stages[0]), ["ls", "/a*", "/b*", "/c?", "\\x"]);
    let globs: Vec<bool> = stages[0].iter().map(|w| w.glob).collect();
    assert_eq!(globs, [false, true, false, false, false]);
    assert_eq!(lexer::lex("a#b c~d").unwrap()[0].len(), 2);
    assert_eq!(lexer::lex("''").unwrap()[0][0].text, "");
}

#[test]
fn lenient_fragments_cut_at_every_operator() {
    assert_eq!(
        lexer::fragments("a 'b c';d|e&&f$(g h)`i`>j<k\nl {m} (n)"),
        [
            vec!["a", "b c"],
            vec!["d"],
            vec!["e"],
            vec!["f"],
            vec!["g", "h"],
            vec!["i"],
            vec!["j"],
            vec!["k"],
            vec!["l"],
            vec!["m"],
            vec!["n"],
        ]
    );
    assert_eq!(
        lexer::fragments("x 'unterminated y"),
        [vec!["x", "unterminated y"]]
    );
    assert!(lexer::fragments("  ;; ").is_empty());
}
