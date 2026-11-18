# ADR-0150: Website checks

Date: 2026-11-18
Status: accepted

## Context

A website can be down while its server looks fine: the application answers `502`, a deploy left an error page, a redirect loops, or the certificate expired overnight. MonitorFlow could only check that a port accepts TCP connections (ADR-0021); everything else needed a script on the host (the "monitor a website" guide). People moving from Zabbix (web scenarios, HTTP agent items), Prometheus (the blackbox exporter) or PRTG (HTTP, HTTP Advanced and SSL certificate sensors) found their web checks approximated as "port answers" by the importer (ADR-0146). The roadmap asks for a check of a page the way a visitor gets it: status code, text expected or not, response time, redirects and the certificate, run by a server's agent or by a probe (ADR-0148).

## Decision

**A new kind, `http`, in a crate of its own.** `crates/webcheck` (`monitorflow-webcheck`) implements the `Sensor` trait of `monitorflow-sensors` for kind `http`. It is not in `monitorflow-sensors`, which keeps no HTTP or TLS client (ADR-0021): the agent, the central server and relays depend on it. A check is an ordinary `sensor_configs` row (ADR-0016) at a server or a site layer, with the usual name, instance, interval and timeout, so layers, revisions, the threshold engine, metrics, silences and dashboards apply unchanged. The interface names it after its instance ("shop"), and the series key is `(server, http_up, instance)`.

**What one run does.** It resolves the host (IPv4, IPv6 or either), connects, speaks TLS for `https`, sends one HTTP/1.1 request (`GET` or `HEAD`) with `Connection: close`, and follows redirects (301, 302, 303, 307, 308, up to 10, http and https only) unless told not to. A `GET` reads at most 1 MiB of the page and looks for the texts in it, as plain case-sensitive text, not a pattern. Nothing is cached between runs. The run stops 200 ms before the collection deadline, like a port check, so a site that hangs reports down rather than nothing.

Parameters, all strings:

| Parameter | Meaning | Default |
| --- | --- | --- |
| `url` | `http://` or `https://`, with a host; no user name or password in it | Required |
| `method` | `GET` or `HEAD` (no text conditions then) | `GET` |
| `status` | Codes and ranges that pass: `200,301-302` | `200-299`, or `200-399` without following redirects |
| `contains`, `not_contains` | Text the page must, or must not, hold | None |
| `follow_redirects` | `true` or `false` | `true` |
| `headers` | `Name: value` lines, at most 16; `Host`, hop-by-hop headers, `Authorization`, `Proxy-Authorization` and `Cookie` refused | None |
| `username`, `password` | Basic authentication; `password` alone is sent as a bearer token | None |
| `ip` | `any`, `4` or `6`, for host names | `any` |

**Metrics**, prefixed with the row's name (`http`):

| Metric | Value |
| --- | --- |
| `up` | 1 when the answer is as expected; 0 with label `reason`: `dns`, `connect`, `timeout`, `tls`, `certificate`, `status`, `content`, `redirects`, `refused` or `protocol`. Label `url` |
| `status_code` | Status of the last answer |
| `response_seconds` | From the start to the end of the page read, redirects included |
| `redirects` | Redirects followed, label `final_url` without its query |
| `content_ok` | 1 when the text conditions pass, when there are some |
| `cert_ok` | 1 when the chain is trusted, valid now and for the name; 0 with `reason`: `expired`, `not_yet_valid`, `self_signed`, `untrusted`, `name`, `revoked` or `other` |
| `cert_name_ok` | 1 when the certificate is for the host asked, whatever else is wrong |
| `cert_days_left` | Days until it expires, negative after; labels `issuer` and `subject` (common names) |
| `cert_not_after` | When it expires, Unix seconds |

The certificate is that of the last https connection of the run, so `http://` redirected to `https://` reports the certificate visitors end on. Labels change rarely (a renewal, a new failure reason), and the threshold engine ignores them, so an incident follows the check across them.

**The certificate is judged, not required.** A verifier wraps rustls' WebPKI verifier against the roots of the machine running the check (the system's, or Mozilla's when it has none), records its verdict and the certificate, and lets the handshake continue; handshake signatures are still verified. The page is then read, so an expired certificate shows as `cert_ok 0` with the status and text still known, rather than a bare TLS failure. TLS 1.2 and 1.3 are accepted: this is a visitor's connection to someone's website, not a MonitorFlow link, whose rules (§4, TLS 1.3 and our CA only) are unchanged. A check that carries a password never sends it over a connection whose certificate failed: it reports `up 0`, `reason="certificate"`, without a request.

**The password is a sealed secret.** It is accepted only with an `https://` URL, never in clear on the network. The server keeps it in `sensor_configs.secret`, sealed like other secrets (ADR-0125, column `sensor_configs.secret`), never in `params`, never in answers, audit details or logs: API answers say `secret_set: true`. An update without `password` keeps the stored one; an empty one removes it. It is opened only when a configuration is sent to whoever runs the check, over mTLS. The agent keeps its last configuration in `sensor-config.pb`, now written with mode 0600. A user name is not secret.

**Who runs it.** A check runs on the agent of its server by default: the site as that server sees it, which also covers a site served from an internal network. It can instead be run by a probe (ADR-0148): the central server or a relay of the site, so a site is checked from outside the machine that serves it, or a device without an agent is given one. The row's `probe` column names it (`probe` on the sensor routes, "Checked from" in the form, this server's agent by default), on a server's or a device's own row only; a device's website checks always run from a probe; agents never receive those rows, and probes build kind `http` beside their network kinds (`monitorflow_webcheck::ProbeKinds`). A probe-run check takes no address from the device: its URL says where to go. Its samples enter the same ingest and threshold path as an agent's, labelled with the server or device it belongs to and the probe that measured it. A relay that runs a check with a password receives it in its configuration, over its mTLS stream, like an agent.

**Where an agent may connect.** An agent applies its `sensors.port_targets` (ADR-0111) to website checks as to port checks: the URL's host and port when the check is applied, and every address and every redirect at each run. A check outside them is refused and shows so; a redirect outside them reports `up 0`, `reason="refused"`.

**Alerts and thresholds.** Adding a website check from the interface adds, at the same layer, the alerts that come with it: critical when `http_up` is 0 for 2 minutes, critical when `http_cert_ok` is 0 for 5 minutes, warning when `http_cert_days_left` is below 14 and critical below 7, and warning when `http_response_seconds` is over 3 for 5 minutes. They are ordinary threshold rows (ADR-0019) with the check's instance, edited in plain words like the others (ADR-0057).

**In the interface.** The check catalogue gets "Website" entries ("A website answers", "A page holds a text", "A certificate does not expire", "An API answers"), all of kind `http` with different fields shown first. The form asks for the URL and offers the rest under "More options"; who runs the check is a choice of "This server's agent" or a probe. The server page lists website checks with their last status, response time, final URL and certificate (issuer, expiry, days left, what is wrong), read from VictoriaMetrics by `GET /api/servers/{id}/website-checks`.

**API.** Rows go through the existing `sensor_configs` routes of `/api/v1`, with `kind: "http"`, documented in the OpenAPI document with the parameters above; the website-checks route is in `/api/v1` too.

**Import.** Zabbix web scenarios (first step's URL, required codes and string, redirects, headers, authentication) and HTTP agent items, Prometheus blackbox `http` and `tcp` with `tls` modules and their rules on `probe_success`, `probe_http_status_code`, `probe_duration_seconds` and `probe_ssl_earliest_cert_expiry`, and PRTG HTTP, HTTP Advanced and SSL certificate sensors become website checks, exact when every setting maps and approximated with the reason otherwise (a POST, a regular expression, steps after the first). Credentials found in an export are not imported: the check is marked approximated, "set its password".

## Consequences

- A site is checked the way a visitor sees it, from the server that serves it, from another server, or from a probe, with no script to install.
- One more kind for agents to know: an agent older than this one refuses `http` rows as an unknown kind, shown in the acknowledgement like any refused row, until it is updated.
- The certificate's issuer and subject are labels, so a renewal by another authority starts new series for `cert_days_left`; the threshold engine and the check page follow it by name and instance.
- The page is read only up to 1 MiB, and text conditions are plain text. Regular expressions, POST bodies and multi-step scenarios are not supported; the importer says so.
- HTTP/2 and HTTP/3 are not used: the check speaks HTTP/1.1, which every web server still answers.
