---
title: "How to run scripts on your servers and automate fixes"
description: "Keep shell and PowerShell scripts in MonitorFlow, run them on a server, a business service or a whole site, now or tonight, read each server's output, and run them by themselves when an incident opens."
slug: run-scripts-and-automate-fixes
lang: en
translation: lancer-des-scripts-et-automatiser
---

# How to run scripts on your servers and automate fixes

Every team has its fixes: clear a cache, rotate a log, restart a stuck service. MonitorFlow keeps them in its script library, runs them on the servers you choose, keeps what each server answered, and can run them by itself when an incident opens.

## Allow scripts on the servers

A server runs library scripts only if its administrator allowed it at install, as the agent's user and never as root. The web interface cannot turn this on, so a central server taken over cannot run code on servers that did not agree.

- **Linux**: add `MF_ALLOW_SCRIPTS=1` to the install command (tick "Allow scripts from the script library" on the Add servers page), or run the installer again on an installed server with it. `MF_ALLOW_SCRIPTS=0` takes it back.
- **Windows**: install the MSI with `ALLOW_SCRIPTS=1`, or run `monitorflow-agent.exe scripts on` as an administrator, then `Restart-Service MonitorFlowAgent`.

The server's Commands tab says whether it accepts library scripts.

## Add a script

Under **Remediation › Scripts**, **New script**:

1. Give it a name, like `clear-app-cache`; the AI and people call it by it.
2. Choose **Shell (Linux)** or **PowerShell (Windows)**, and the least role that may run it: operator, approver or administrator.
3. Write the script. Add parameters with a type (text, whole number, yes or no, choice) and a default; a value reaches the script as the environment variable `MF_PARAM_<NAME>`, for example `"$MF_PARAM_DAYS"` in shell or `$env:MF_PARAM_DAYS` in PowerShell. It is never pasted into the script's text.
4. Tick **The AI may propose it as a fix** if AI remediation may use it.

Each change of the text or of the parameters makes a new version; earlier ones stay on the script's page, and a run names the version it ran.

## Run it

On the script's page, **Run…**: choose servers, a whole site or a business service, give the values, and run now or pick a time. MonitorFlow classifies the script with the same risk rules as AI commands, line by line:

- a line the rules forbid, like `rm -rf /`, refuses the run;
- a script of read-only commands (`df -h`, `free -m`) is read-only; anything the rules do not know, loops and variables included, is high impact.

The run starts at once when your role may approve that risk on every server's site (approver for read-only and low impact, administrator for high impact). Otherwise it waits under **Remediation › Approvals**, in "Scripts waiting for approval", for up to 24 hours.

The run's page shows each server's state, exit code, output and errors (the last 64 KiB). A server that refuses scripts, runs the other system, or was removed is skipped, and the page says why and how to allow scripts there.

## Run it when something happens

Under **Remediation › Automation**, **New rule**: choose where it applies (everywhere, a customer or a site), when it runs (an incident opens, of any metric or of one; a check fails; a server joins), the script and its values. The rule runs the script on the server where it happened:

- read-only scripts run at once, low-impact ones when the site lets AI changes run on their own, the others wait for approval like an AI command;
- at most once per server per cooldown (60 minutes by default), and 50 times per rule in an hour;
- "Show what it did" lists each event it took, with its run or why it skipped it.

## Let the AI use them

Scripts marked for the AI are listed in its instructions during remediation. The AI proposes one by name with its values, and MonitorFlow classifies, approves and runs it exactly as any AI command, within the incident's limits. Its output goes back to the AI and into the audit log.

## Over the API

Everything above is in the public API under `/api/v1/remediation/scripts`, `/api/v1/remediation/script-runs` and `/api/v1/remediation/automation-rules`, described in the API documentation.
