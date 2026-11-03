//! Table-driven tests of the PowerShell rules: the built-in tiers, every
//! syntax rule, the Windows deny rules, and bypass attempts.

use uuid::Uuid;

use super::lexer::{self, Word};
use crate::remediation::risk::{
    classify_for, AllowRule, DenyRule, Pattern, Rule, RuleSet, Shell, Tier,
};

use Tier::{Forbidden as F, HighImpact as H, LowImpact as L, ReadOnly as R};

const ALLOW_RO: Uuid = Uuid::from_u128(1);
const ALLOW_LOW: Uuid = Uuid::from_u128(2);
const DENY: Uuid = Uuid::from_u128(3);

fn rules() -> RuleSet {
    let p = |text| Pattern::parse(text).unwrap();
    RuleSet {
        allow: vec![
            AllowRule {
                id: ALLOW_RO,
                pattern: p("Get-AppHealth **"),
                tier: R,
            },
            AllowRule {
                id: ALLOW_LOW,
                pattern: p("Reset-AppCache"),
                tier: L,
            },
            // Names a cmdlet the built-in rules decide: never applies.
            // Rules are not refused for naming one, as they serve Linux
            // servers too, where `curl` or `net` mean something else.
            AllowRule {
                id: ALLOW_RO,
                pattern: p("Stop-Service **"),
                tier: R,
            },
        ],
        deny: vec![DenyRule {
            id: DENY,
            pattern: p("Restart-Service MSSQL*"),
        }],
        cache_dirs: Vec::new(),
    }
}

fn check(cases: &[(&str, Tier, &str)]) {
    let rules = rules();
    let mut failures = Vec::new();
    for &(command, tier, rule) in cases {
        let decision = classify_for(Shell::PowerShell, command, &rules);
        let id = match decision.rule {
            Rule::Builtin { id } => id.to_owned(),
            Rule::Configured { id } => format!("configured:{}", id.as_u128()),
        };
        if decision.tier != tier || id != rule {
            failures.push(format!(
                "{command:?}: got {:?} {id} ({}), want {tier:?} {rule}",
                decision.tier, decision.reason
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn read_only_diagnosis() {
    check(&[
        ("Get-Service", R, "readonly.cmdlet"),
        ("Get-Service -Name W3SVC,WAS", R, "readonly.cmdlet"),
        ("get-service w3svc", R, "readonly.cmdlet"),
        ("gsv 'World Wide Web*'", R, "readonly.cmdlet"),
        ("Get-Process | Sort-Object CPU -Descending | Select-Object -First 10", R, "readonly.cmdlet"),
        ("ps -Name sqlservr", R, "readonly.cmdlet"),
        ("Get-Volume", R, "readonly.cmdlet"),
        ("Get-PSDrive -PSProvider FileSystem", R, "readonly.cmdlet"),
        ("Get-CimInstance Win32_LogicalDisk -Filter \"DriveType=3\"", R, "readonly.cmdlet"),
        ("Get-CimInstance -ClassName Win32_OperatingSystem | Format-List", R, "readonly.cmdlet"),
        ("Get-WmiObject Win32_Service -Filter \"State='Stopped'\"", R, "readonly.cmdlet"),
        ("Get-WinEvent -LogName System -MaxEvents 50", R, "readonly.cmdlet"),
        ("Get-WinEvent -LogName Application -MaxEvents 20 | Where-Object LevelDisplayName -eq Error | Format-List TimeCreated,ProviderName,Message", R, "readonly.cmdlet"),
        ("Get-WinEvent -LogName System -FilterXPath '*[System[(Level=1 or Level=2)]]' -MaxEvents 30", R, "readonly.cmdlet"),
        ("Get-EventLog -LogName System -EntryType Error -Newest 20", R, "readonly.cmdlet"),
        ("Get-Counter '\\Processor(_Total)\\% Processor Time' -SampleInterval 2 -MaxSamples 3", R, "readonly.cmdlet"),
        ("Get-NetTCPConnection -State Listen | Sort-Object LocalPort", R, "readonly.cmdlet"),
        ("Get-IISAppPool", R, "readonly.cmdlet"),
        ("Get-WebAppPoolState -Name DefaultAppPool", R, "readonly.cmdlet"),
        ("Get-HotFix | Sort-Object InstalledOn -Descending | Select-Object -First 5", R, "readonly.cmdlet"),
        ("Get-ScheduledTask | Where-Object State -eq Running", R, "readonly.cmdlet"),
        ("Get-ChildItem C:\\inetpub\\logs\\LogFiles -Recurse -File | Sort-Object Length -Descending | Select-Object -First 20 FullName,Length", R, "readonly.list"),
        ("Get-ChildItem -Path D:\\Data -Force", R, "readonly.list"),
        ("dir C:\\Windows\\Temp", R, "readonly.list"),
        ("Get-ChildItem", R, "readonly.list"),
        ("Get-Item C:\\pagefile.sys -Force", R, "readonly.list"),
        ("Get-Content C:\\inetpub\\logs\\LogFiles\\W3SVC1\\u_ex261103.log -Tail 100", R, "readonly.log_file"),
        ("Get-Content -Path C:\\Windows\\Logs\\CBS\\CBS.log -Tail 200 | Select-String error", R, "readonly.log_file"),
        ("gc 'C:\\Windows\\System32\\LogFiles\\HTTPERR\\httperr1.log' -Tail 50", R, "readonly.log_file"),
        ("Get-Content C:\\inetpub\\logs\\LogFiles\\W3SVC1\\*.log -Tail 20", R, "readonly.log_file"),
        ("Select-String -Path C:\\Windows\\Logs\\CBS\\CBS.log -Pattern 'error' -SimpleMatch", R, "readonly.log_file"),
        ("Get-Process | Measure-Object WorkingSet -Sum", R, "readonly.cmdlet"),
        ("Get-Service | Group-Object Status -NoElement", R, "readonly.cmdlet"),
        ("Get-Service | ConvertTo-Json -Depth 2", R, "readonly.cmdlet"),
        ("Get-Process | Format-Table -AutoSize | Out-String -Width 200", R, "readonly.cmdlet"),
        ("hostname", R, "readonly.program"),
        ("whoami /all", R, "readonly.program"),
        ("ipconfig /all", R, "readonly.program"),
        ("ipconfig.exe", R, "readonly.program"),
        ("systeminfo /fo list", R, "readonly.program"),
        ("tasklist /svc /fi 'IMAGENAME eq w3wp.exe'", R, "readonly.program"),
        ("netstat -ano", R, "readonly.program"),
        ("netstat -an -p TCP", R, "readonly.program"),
        ("sc.exe query W3SVC", R, "readonly.sc"),
        ("sc.exe qc MSSQLSERVER", R, "readonly.sc"),
        ("wevtutil qe System /c:20 /rd:true /f:text", R, "readonly.wevtutil"),
        ("wevtutil el", R, "readonly.wevtutil"),
        ("w32tm /query /status", R, "readonly.program"),
    ]);
}

#[test]
fn low_impact_changes() {
    check(&[
        ("Restart-Service W3SVC", L, "low.service"),
        ("Restart-Service -Name Spooler", L, "low.service"),
        ("Start-Service MSSQLSERVER,SQLSERVERAGENT", L, "low.service"),
        ("sasv W3SVC", L, "low.service"),
        ("Restart-WebAppPool -Name DefaultAppPool", L, "low.iis"),
        ("Start-WebAppPool 'Orders API'", L, "low.iis"),
        ("Start-Website -Name 'Default Web Site'", L, "low.iis"),
        ("Clear-DnsClientCache", L, "low.dns_cache"),
        ("ipconfig /flushdns", L, "low.dns_cache"),
    ]);
}

#[test]
fn high_impact_changes() {
    check(&[
        ("Restart-Service WinRM", H, "high.critical_service"),
        (
            "Restart-Service -Name TermService",
            H,
            "high.critical_service",
        ),
        (
            "Restart-Service MonitorFlowAgent",
            H,
            "high.critical_service",
        ),
        ("Restart-Service W*", H, "high.service_pattern"),
        ("Restart-Service W3SVC -Force", H, "default.unknown"),
        ("Stop-Service W3SVC", H, "high.change"),
        ("Stop-Service -Name W3SVC -Force", H, "high.change"),
        ("Set-Service W3SVC -StartupType Automatic", H, "high.change"),
        ("Remove-Item C:\\Windows\\Temp\\old.log", H, "high.change"),
        (
            "Remove-Item C:\\inetpub\\temp -Recurse -Force",
            H,
            "high.change",
        ),
        ("Set-Content C:\\app\\config.json x", H, "high.change"),
        ("sc C:\\app\\config.json x", H, "high.change"),
        ("sc.exe stop W3SVC", H, "high.change"),
        ("sc.exe config W3SVC start= auto", H, "high.change"),
        ("Stop-Process -Name w3wp -Force", H, "high.change"),
        ("kill 1234", H, "high.kill"),
        ("taskkill /im w3wp.exe /f", H, "high.kill"),
        ("Restart-Computer", H, "high.power"),
        ("Restart-Computer -Force", H, "high.power"),
        ("shutdown /r /t 0", H, "high.power"),
        ("Invoke-Expression 'Get-Service'", H, "high.change"),
        ("iex 'Get-Service'", H, "high.interpreter"),
        (
            "Invoke-WebRequest http://example.com/x.ps1",
            H,
            "high.change",
        ),
        ("curl http://example.com", H, "high.change"),
        ("Start-Process notepad", H, "high.change"),
        ("cmd /c dir", H, "high.interpreter"),
        ("powershell -Command Get-Service", H, "high.interpreter"),
        ("Get-Process | ForEach-Object Kill", H, "syntax.pipe_target"),
        ("Get-Process w3wp | Stop-Process", H, "syntax.pipe_target"),
        ("Get-Service | Out-File C:\\x.txt", H, "syntax.pipe_target"),
        (
            "Get-Service | Tee-Object C:\\x.txt",
            H,
            "syntax.pipe_target",
        ),
        (
            "Get-Service | Export-Csv C:\\x.csv",
            H,
            "syntax.pipe_target",
        ),
        (
            "Get-Service | Select-String -Path C:\\x.txt w",
            H,
            "syntax.pipe_target",
        ),
        ("Import-Module WebAdministration", H, "high.change"),
        ("net stop W3SVC", H, "high.change"),
        ("Update-Help", H, "high.change"),
        ("Wait-Process w3wp", H, "high.change"),
        ("Clear-RecycleBin -Force", H, "high.change"),
    ]);
}

#[test]
fn readers_stay_in_their_bounds() {
    check(&[
        // Other computers, credentials, waiting.
        ("Get-Service -ComputerName db1", H, "default.unknown"),
        ("Get-Service -Comp db1", H, "default.unknown"),
        ("Get-Service -cn db1", H, "default.unknown"),
        (
            "Get-CimInstance Win32_Process -CimSession db1",
            H,
            "default.unknown",
        ),
        (
            "Get-WmiObject Win32_Process -EnableAllPrivileges",
            H,
            "default.unknown",
        ),
        ("Get-WinEvent -Path C:\\x.evtx", H, "default.unknown"),
        (
            "Get-Counter '\\Processor(_Total)\\% Processor Time' -Continuous",
            H,
            "default.unknown",
        ),
        ("Get-Service \\\\db1\\x", H, "default.unknown"),
        // Files outside the log directories, or not plainly named.
        ("Get-Content C:\\app\\secrets.json", H, "default.unknown"),
        (
            "Get-Content C:\\Windows\\Logs\\..\\System32\\config\\SAM",
            H,
            "default.unknown",
        ),
        (
            "Get-Content C:\\Windows\\Logs\\x.log:secret",
            H,
            "default.unknown",
        ),
        ("Get-Content C:\\Windows\\Lo*\\x.log", H, "default.unknown"),
        ("Get-Content Env:PATH", H, "default.unknown"),
        ("Get-Content HKLM:\\SOFTWARE\\x", H, "default.unknown"),
        ("Get-Content \\\\server\\share\\x.log", H, "default.unknown"),
        ("Get-Content C:\\Windows\\Logs", H, "default.unknown"),
        (
            "Get-Content C:\\Windows\\Logs\\CBS\\CBS.log -Wait",
            H,
            "default.unknown",
        ),
        (
            "Get-Content C:\\Windows\\Logs\\CBS\\CBS.log -Tail x",
            H,
            "default.unknown",
        ),
        (
            "Get-Content C:\\Windows\\Logs\\CBS\\CBS.log -Stream s",
            H,
            "default.unknown",
        ),
        (
            "Get-Content C:\\Windows\\Logs\\CBS\\CBS.log -Ta 5",
            H,
            "default.unknown",
        ),
        (
            "Get-ChildItem Cert:\\LocalMachine\\My",
            H,
            "default.unknown",
        ),
        ("Get-ChildItem Env:", H, "default.unknown"),
        ("Get-ChildItem ~", H, "default.unknown"),
        ("Get-ChildItem C:\\ -Exec x", H, "default.unknown"),
        ("Get-Item HKLM:\\SOFTWARE", H, "default.unknown"),
        // Programs with arguments that change or never end.
        ("ipconfig /release", H, "high.change"),
        ("netstat -an 5", H, "default.unknown"),
        ("tasklist /s db1 /u admin /p x", H, "default.unknown"),
        ("hostname newname", H, "default.unknown"),
        ("wevtutil qe System /r:db1", H, "high.change"),
        ("wevtutil epl System C:\\x.evtx", H, "high.change"),
        ("w32tm /resync", H, "high.change"),
        (
            "Get-Process | Where-Object -FilterScript x",
            H,
            "syntax.pipe_target",
        ),
        (
            "Get-Process | Select-Object -Property Name -Wait",
            H,
            "syntax.pipe_target",
        ),
    ]);
}

#[test]
fn syntax_is_refused() {
    check(&[
        ("Get-Service; Restart-Computer", H, "syntax.command_list"),
        ("Get-Service & Restart-Computer", H, "syntax.command_list"),
        ("Get-Service && hostname", H, "syntax.command_list"),
        ("Get-Service || hostname", H, "syntax.command_list"),
        ("Get-Service\nhostname", H, "syntax.command_list"),
        ("Get-Service\rhostname", H, "syntax.control"),
        ("Get-Service $env:X", H, "syntax.substitution"),
        ("Get-Service \"$(hostname)\"", H, "syntax.substitution"),
        ("Get-Service \"a`b\"", H, "syntax.escape"),
        ("G`et-Service", H, "syntax.escape"),
        ("Get-Service (hostname)", H, "syntax.grouping"),
        ("Get-Service @args", H, "syntax.grouping"),
        (
            "Get-Service | Where-Object { $_.Status }",
            H,
            "syntax.grouping",
        ),
        ("& 'C:\\x.exe'", H, "syntax.command_list"),
        (". C:\\x.ps1", H, "syntax.command_name"),
        ("[IO.File]::ReadAllText('C:\\x')", H, "syntax.type"),
        ("Get-Service > C:\\x.txt", H, "syntax.redirection"),
        ("Get-Service 2>&1", H, "syntax.redirection"),
        ("Get-Service 2> x", H, "syntax.redirection"),
        ("Get-Service # comment", H, "syntax.comment"),
        ("Get-Service 'abc", H, "syntax.unterminated"),
        ("Get-Service a'b'", H, "syntax.quoting"),
        ("Get-Service 'a'b", H, "syntax.quoting"),
        ("Get-Service --% x", H, "syntax.stop_parsing"),
        ("'Get-Service'", H, "syntax.command_name"),
        ("C:\\tools\\x.exe", H, "syntax.command_name"),
        (".\\x.ps1", H, "syntax.command_name"),
        ("Get-Service |", H, "syntax.pipe"),
        ("Get\u{2013}Service", H, "syntax.non_ascii"),
        ("Get-Service \u{2018}x\u{2019}", H, "syntax.non_ascii"),
        ("Get-Service\u{0}", H, "syntax.control"),
        (
            "Microsoft.PowerShell.Management\\Get-Service",
            H,
            "syntax.command_name",
        ),
    ]);
}

#[test]
fn forbidden_on_windows() {
    check(&[
        // Disks.
        ("Format-Volume -DriveLetter D", F, "forbidden.format"),
        ("format D: /q", F, "forbidden.format"),
        ("Clear-Disk -Number 1 -RemoveData", F, "forbidden.format"),
        ("Initialize-Disk 2", F, "forbidden.format"),
        ("diskpart /s C:\\x.txt", F, "forbidden.format"),
        ("Get-Content \\\\.\\PhysicalDrive0", F, "forbidden.format"),
        // System files.
        ("Remove-Item C:\\Windows -Recurse -Force", F, "forbidden.system_files"),
        ("Remove-Item -Path C:\\Windows\\System32\\drivers\\x.sys", F, "forbidden.system_files"),
        ("rm -r -fo C:\\", F, "forbidden.system_files"),
        ("Remove-Item C:\\* -Recurse", F, "forbidden.system_files"),
        ("ri 'C:\\Program Files' -Recurse", F, "forbidden.system_files"),
        ("Remove-Item C:\\PROGRA~1 -Recurse", F, "forbidden.system_files"),
        ("Remove-Item C:\\W*\\Sys*\\x.dll", F, "forbidden.system_files"),
        ("Remove-Item Windows -Recurse", F, "forbidden.system_files"),
        ("Remove-Item \\Users -Recurse", F, "forbidden.system_files"),
        ("Remove-Item C:/Windows/System32/x.dll", F, "forbidden.system_files"),
        ("Remove-Item $env:SystemRoot -Recurse", F, "forbidden.system_files"),
        ("Remove-Item \"$env:windir\\System32\\x\"", F, "forbidden.system_files"),
        ("Remove-Item ${env:ProgramFiles(x86)} -Recurse", F, "forbidden.system_files"),
        ("Remove-Item C:\\temp\\..\\Windows -Recurse", F, "forbidden.system_files"),
        ("cmd /c \"rd /s /q C:\\Windows\"", F, "forbidden.system_files"),
        ("cmd /c del /s /q %SystemRoot%\\System32\\*", F, "forbidden.system_files"),
        ("R`emove-Item C:\\Windows -Recurse", F, "forbidden.system_files"),
        ("takeown /f C:\\Windows\\System32\\x.dll", F, "forbidden.system_files"),
        ("icacls C:\\Windows\\System32 /grant Everyone:F", F, "forbidden.system_files"),
        // Defender and firewall.
        ("Set-MpPreference -DisableRealtimeMonitoring $true", F, "forbidden.defender"),
        ("set-mppreference -drtm 1", F, "forbidden.defender"),
        ("Add-MpPreference -ExclusionPath C:\\", F, "forbidden.defender"),
        ("Stop-Service WinDefend", F, "forbidden.defender"),
        ("Get-Service WinDefend | Stop-Service -Force", F, "forbidden.defender"),
        ("Set-Service -Name WinDefend -StartupType Disabled", F, "forbidden.defender"),
        ("sc.exe config WinDefend start= disabled", F, "forbidden.defender"),
        ("net stop mpssvc", F, "forbidden.defender"),
        ("taskkill /f /im MsMpEng.exe", F, "forbidden.defender"),
        ("Set-NetFirewallProfile -Profile Domain -Enabled False", F, "forbidden.defender"),
        ("netsh advfirewall set allprofiles state off", F, "forbidden.defender"),
        ("Uninstall-WindowsFeature Windows-Defender", F, "forbidden.defender"),
        ("reg add HKLM\\SOFTWARE\\Policies\\Microsoft\\Windows Defender /v DisableAntiSpyware /d 1", F, "forbidden.defender"),
        // Accounts and credentials.
        ("New-LocalUser -Name x", F, "forbidden.accounts"),
        ("Add-LocalGroupMember -Group Administrators -Member x", F, "forbidden.accounts"),
        ("net user admin NewPass1 /add", F, "forbidden.accounts"),
        ("net localgroup administrators x /add", F, "forbidden.accounts"),
        ("Set-ADAccountPassword -Identity x", F, "forbidden.accounts"),
        ("cmdkey /add:x /user:y /pass:z", F, "forbidden.accounts"),
        ("reg save HKLM\\SAM C:\\sam.hiv", F, "forbidden.accounts"),
        ("rundll32 comsvcs.dll MiniDump 624 C:\\lsass.dmp full", F, "forbidden.accounts"),
        // Backups and boot.
        ("vssadmin delete shadows /all /quiet", F, "forbidden.recovery"),
        ("wmic shadowcopy delete", F, "forbidden.recovery"),
        ("Get-CimInstance Win32_ShadowCopy | Remove-CimInstance", F, "forbidden.recovery"),
        ("wbadmin delete catalog -quiet", F, "forbidden.recovery"),
        ("bcdedit /set {default} recoveryenabled no", F, "forbidden.recovery"),
        // Event logs.
        ("Clear-EventLog -LogName Security", F, "forbidden.event_logs"),
        ("wevtutil cl System", F, "forbidden.event_logs"),
        ("wevtutil sl Security /e:false", F, "forbidden.event_logs"),
        // Hidden commands.
        ("powershell -EncodedCommand ZQBjAGgAbwA=", F, "forbidden.encoded"),
        ("powershell.exe -enc ZQBjAGgAbwA=", F, "forbidden.encoded"),
        ("pwsh -e ZQBjAGgAbwA=", F, "forbidden.encoded"),
        ("iex ([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('eA==')))", F, "forbidden.encoded"),
        // Administrator deny rules, without regard to case.
        ("restart-service mssqlserver", F, "configured:3"),
        ("Restart-Service MSSQLSERVER", F, "configured:3"),
    ]);
}

#[test]
fn administrator_rules_extend_unknown_commands_only() {
    check(&[
        ("Get-AppHealth -Verbose", R, "configured:1"),
        ("Reset-AppCache", H, "high.change"),
        ("Stop-Service W3SVC", H, "high.change"),
        ("Get-AppHealth; Restart-Computer", H, "syntax.command_list"),
        ("Get-AppHealth | Stop-Process", H, "syntax.pipe_target"),
    ]);
}

#[test]
fn not_forbidden_by_accident() {
    check(&[
        ("Get-Service WinDefend", R, "readonly.cmdlet"),
        ("Get-MpComputerStatus", R, "readonly.cmdlet"),
        ("Get-Date -Format o", R, "readonly.cmdlet"),
        ("Remove-Item C:\\Windows\\Temp\\x.tmp", H, "high.change"),
        (
            "Remove-Item C:\\Windows\\SoftwareDistribution\\Download -Recurse",
            H,
            "high.change",
        ),
        ("Get-Process | Format-List", R, "readonly.cmdlet"),
        ("net user", H, "high.change"),
    ]);
}

#[test]
fn the_lexer_keeps_quoted_words_whole() {
    let words = |text| lexer::lex(text).unwrap();
    assert_eq!(
        words("Get-Content 'C:\\a b\\it''s.log' -Tail 5")[0],
        [
            Word {
                text: "Get-Content".into(),
                quoted: false
            },
            Word {
                text: "C:\\a b\\it's.log".into(),
                quoted: true
            },
            Word {
                text: "-Tail".into(),
                quoted: false
            },
            Word {
                text: "5".into(),
                quoted: false
            },
        ]
    );
    // A quoted parameter name is a string, never a parameter.
    let quoted = &words("Get-Service '-ComputerName' db1")[0][1];
    assert!(quoted.parameter().is_none());
    assert_eq!(
        words("Get-Service -Name:W3SVC")[0][1].parameter(),
        Some(("name".into(), Some("W3SVC")))
    );
    assert_eq!(words("a | b | c").len(), 3);
}

#[test]
fn the_same_text_is_classified_by_its_shell() {
    let rules = RuleSet::default();
    // Read-only under sh, a file write in Windows PowerShell (ADR-0047).
    let sc = "sc query W3SVC";
    assert_eq!(classify_for(Shell::PowerShell, sc, &rules).tier, H);
    // Read-only in PowerShell, unknown under sh.
    let gsv = "Get-Service";
    assert_eq!(classify_for(Shell::PowerShell, gsv, &rules).tier, R);
    assert_eq!(classify_for(Shell::Posix, gsv, &rules).tier, H);
    assert_eq!(classify_for(Shell::Posix, "df -h", &rules).tier, R);
    assert_eq!(classify_for(Shell::PowerShell, "df -h", &rules).tier, H);
    assert_eq!(Shell::of("windows"), Shell::PowerShell);
    assert_eq!(Shell::of("debian"), Shell::Posix);
    assert_eq!(Shell::of("unknown"), Shell::Posix);
}
