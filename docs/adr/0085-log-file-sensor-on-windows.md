# ADR-0085: The log file sensor on Windows, and positions kept across restarts

Date: 2026-11-03
Status: accepted; amends ADR-0024 (log reading) and ADR-0048 (follow-up done)

## Context

The `log` sensor of ADR-0024 counts lines matching a pattern in a text file under the agent's log directories, and follows the file through rotation by its inode. ADR-0048 left it Unix only. Windows servers keep text logs too (IIS, SQL Server's error log, most application frameworks), and Windows differs in what makes this work: there are no inodes, opening a file can lock its writer out, many tools write UTF-16, lines end with CRLF, and paths have drive letters, UNC shares and no case. The sensor also forgot where it was when the agent restarted or its configuration changed (a consequence listed in ADR-0024), which on Windows happens at every self-update.

## Decision

The `log` kind runs on both systems with the same parameters, metrics (`matches`, `lag_bytes`) and failure semantics. The reading code is shared. Only opening, file identity and reading at an offset are per system, in `log/unix.rs` and `log/windows.rs`.

### Paths

`path` is a Linux path (`/var/log/app.log`) or a Windows one (`C:\Logs\app.log`). The server validates rows for every agent with `registry::build` (ADR-0048), so both forms are accepted on both systems. An agent refuses the other system's form ("C:\Logs\app.log is a Windows path"). A Windows path is a drive letter, `:\`, and segments separated by backslashes. Segments that Windows would change or read as something else are refused: empty, `.` or `..`, ending with a dot or space, containing `<>:"/|?*` (so no alternate data stream and no wildcard), or a device name (`CON`, `NUL`, `COM1`, `LPT1`, …).

- **UNC paths** (`\\server\share\…`) and device paths (`\\?\`, `\\.\`) are refused. The agent's virtual account reaches shares as the computer account, a share can hang a blocking read for minutes, and SMB file IDs are not reliable enough to follow rotation. A file on a share is watched by the agent of the server that writes it. A link inside an allowed folder that leads to a share resolves to `\\?\UNC\…`, which is outside the allowed folders, so it is refused like any other escape.
- **Case**: the configured path and `sensors.log_dirs` are compared without case on Windows, as NTFS does. Resolved paths come from the file system with their real case.
- **Long paths**: the configured path is at most 256 bytes, as it is a label value (ADR-0024). Resolution uses `canonicalize`, which returns `\\?\` paths that are not limited to 260 characters, so a link or junction to a deep folder works.
- **Wildcards**: not supported, as on Linux. IIS names its files by date (`u_ex261103.log`), so it cannot be followed yet. Wildcards on both systems are a follow-up.

**Allowed folders.** `sensors.log_dirs` works as on Linux: the path and the folders are resolved and compared at every collection. On Windows the check is done again on the open handle (`GetFinalPathNameByHandleW`), so a link swapped in between the check and the open is caught, which Linux cannot do before `openat2` (ADR-0024). Windows has no common folder of text logs, so the default list is empty. The agent then refuses log checks with "this agent reads no log files: its sensors.log_dirs is empty", and an administrator lists the folders in `agent.toml`. A default like `C:\ProgramData` would include the agent's own key and other services' secrets.

### Which file

A file is identified by its volume serial number and file index instead of device and inode. `GetFileInformationByHandleEx(FileIdInfo)` gives the 64-bit serial number and the 128-bit ID that ReFS needs. On file systems without it (FAT), `GetFileInformationByHandle` gives the 32-bit serial number and the 64-bit index. The ID of the file the path names now is read through a handle opened for attributes only, which no sharing mode refuses.

Identities can be reused: an inode freed by a deletion can be given to the next file, so a file truncated and written again looks unchanged. The sensor therefore also keeps a hash (FNV-1a) of the first 256 bytes of the file it reads, extended as the file grows to 256 bytes. When those bytes change, the file was rewritten. It is then read again from its start, like a file shorter than the offset.

### Opening without locking out the writer

The file is opened with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`. The writer can then keep writing, rename the file away, truncate it and delete it while the sensor holds it. A writer that opened the file without read sharing cannot be read at all. The collection then fails, transient, with "C:\…\app.log is locked by the program writing it, which lets nobody else read it".

Without POSIX delete semantics (older Windows versions, and file systems other than NTFS), a deleted file keeps its name while any handle is open, so the writer could not create it again. The sensor checks whether the file it holds was deleted (`DeletePending`, or no links left on Linux), reads what is left, and closes it. On Linux this also stops it from keeping a deleted file's disk space in use.

### Rotation

- **Rename and create** (log4net, NLog, logrotate's default): as in ADR-0024, the old file is read to its end, then the new one from its start. The old file is now also kept open and read once more at the next collection, as long as it still grows, since a writer often finishes its last lines after the rename. It is dropped at the first collection that finds nothing new. Lines are counted once: each file has its own offset.
- **Rotated twice in one interval**: the file in between is read only if the sensor had opened it.
- **Copy and truncate**: the file is read again from its start when it is shorter than the offset or its first bytes changed. Lines written between the last collection and the copy are only in the copy, which the sensor does not read. This loss is inherent to copytruncate.

### Encodings

The first bytes of each file decide. With the UTF-8 byte order mark, it is skipped. With the UTF-16 LE mark (`FF FE`, what PowerShell 5.1's `Out-File` and many Windows tools write) or the BE one, lines are split on the UTF-16 newline and decoded to UTF-8 before matching, with invalid units replaced by U+FFFD. A code unit written in part waits for its second byte. Any other file is read as bytes, as on Linux. This covers UTF-8 without a mark, ASCII and the Windows code pages for ASCII patterns; a pattern with accented letters only matches UTF-8. UTF-16 without a mark is not detected, because guessing would misread binary or code page files: its ASCII lines then contain NUL bytes and match nothing. A trailing CR is removed from every line, so CRLF files match `error$`. `MAX_LINE` counts code units (4096 in UTF-16 too), and `lag_bytes` counts bytes in every encoding.

### Positions kept across restarts, on both systems

The agent gives the sensor `<data_dir>/log-positions` (`Local::log_positions`). After each collection, the sensor writes the file ID, the start of the first line not counted yet, and the hash of the first bytes to `<FNV-1a of path, pattern and exclude>.pos`, through a temporary file and a rename. It writes when the position changes, and every hour otherwise. Two sensors on the same file with different patterns keep separate positions. A failed write is logged once and does not fail the collection. `monitorflow-agent sensors probe` does not save positions.

On its first collection, a sensor with a saved position:

- resumes at that position if the path still names the same file (same ID, offset within the file, same first bytes). Lines written while the agent was stopped, or during a configuration change, are counted, including a line that was split at the stop.
- reads the file from its start if it is another file. The log was rotated or rewritten while the agent was stopped, so all of the new file was written since. What was left unread in the old file is lost.
- starts at the end, as before, if there is no position or it was saved more than a day ago. This way, a sensor removed and added back weeks later does not count old lines.

At most 1 MiB is still read per collection: a long outage shows as `lag_bytes` and is caught up over the next collections.

### Errors

Access denied, when resolving or opening the file, is now permanent on both systems: "C:\Logs\app.log: access denied; give NT SERVICE\MonitorFlowAgent read access to it" (on Linux, "the agent's user"). As for other permanent errors, the agent tries the sensor again every five minutes (ADR-0024's amendment), so granting access is enough. Other errors keep their ADR-0024 semantics.

## Consequences

- The `log` check is offered for Windows servers in the web interface, with a Windows example and a hint about `log_dirs`. For a site, the hint covers both systems.
- On Windows, an administrator must list log folders in `agent.toml` and may need to give `NT SERVICE\MonitorFlowAgent` read access to them, as application folders under `C:\ProgramData` often grant read access to administrators only.
- On Linux, the sensor behaves differently in three ways. It counts lines written while the agent was stopped. It counts lines written to the old file after a rotation. It reads again a file rewritten past its old offset. Thresholds may fire on these lines, which were silently missed before.
- The agent's data directory gains a small file per log sensor, which is never removed, even when the sensor is. Each file is about 60 bytes.
- When a stopped sensor is retried (ADR-0024's amendment), the probe collection's samples are dropped. If that collection resumes from a saved position, its matches are lost.
- Tests run on both systems in CI: rotation by rename with a late writer, copytruncate past the old end, CRLF and BOM, UTF-16 LE and BE written in pieces, restart, deletion, access denied. On Windows they also cover a writer sharing read only, then rename, truncate and delete while the sensor holds the file, and paths without case.
- Follow-ups: wildcards in file names (dated IIS files), UTF-16 without a mark if it shows up, removing the positions of removed sensors.
