# Static demo of the web interface

The web interface can be built as a read-only demo that needs no server: every API call is answered in the browser by made-up data, and every change is refused. It lives in this repository so the demo follows the interface as it changes.

## Building it

```
cd web
npm ci
npm run build:demo   # static files in web/out
```

`npm run build:demo` is the normal build with `NEXT_PUBLIC_MONITORFLOW_DEMO=1`. Two environment variables are read at build time:

| Variable | Effect |
| --- | --- |
| `NEXT_PUBLIC_MONITORFLOW_DEMO` | `1` builds the demo. `npm run build:demo` sets it. |
| `NEXT_PUBLIC_MONITORFLOW_SITE_URL` | Where the banner's "Website" link leads. Default `https://monitorflow.eu`. |

To look at it, serve `web/out` with any static file server, for example `python3 -m http.server -d web/out 4173`.

## How it works

- `request` in `web/src/lib/api.ts` hands every call to `web/src/lib/demo/handler.ts` when the flag is on. Pages are not changed, so a new page works in the demo as soon as its reads are answered there.
- The data is in `web/src/lib/demo/`: about thirty servers on two sites with one relay, a few open incidents, an AI proposal waiting for approval, the audit log, an update campaign in its second wave, mirrors, notification channels, an on-call rotation and a dashboard. Times are relative to when the page loads, and metric curves are worked out from the time (a daily cycle plus noise), so the demo never looks stale.
- Addresses are in documentation ranges (192.0.2.0/24, 198.51.100.0/24, 203.0.113.0/24) and names under `example.com`.
- The visitor is signed in as `demo`, an administrator of everything. Sign-in, setup and password pages send to the fleet.
- Every POST, PUT, PATCH and DELETE fails with "This is a demo: changes are not saved.", shown like any other API error, in the reader's language. The few POSTs that only read (dashboard data) are answered, and the person's preferences are kept for the tab so the theme and language can be switched.
- A read the demo does not answer gets a 404, which pages show as an error. When a new endpoint is added, add its answer to `handler.ts`.
- Without the flag, the build inlines an empty value: the demo branch, its data and its banner are compiled away, and the normal build behaves as before.

## Where it is published

The website repository, `Kidoly/monitorflow-website`, checks this repository out, runs `npm run build:demo` in `web/` with `NEXT_PUBLIC_MONITORFLOW_SITE_URL` set to the site's address, and publishes `web/out` as static files at `demo.<domain>`.
