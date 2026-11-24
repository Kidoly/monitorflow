// MonitorFlow's service worker (ADR-0186). It only shows the notifications
// the server pushes and acts on their buttons; it caches nothing, so the
// web interface stays exactly what the server serves.
//
// A notification carries no secret: "Acknowledge" goes through this
// phone's own session, and anything else opens the app.

const WORDS = {
  en: {
    acknowledge: "Acknowledge",
    silence: "Silence…",
    acknowledged: "Acknowledged: nobody else is told next.",
  },
  fr: {
    acknowledge: "Prendre en charge",
    silence: "Mettre en silence…",
    acknowledged: "Pris en charge : personne d’autre n’est prévenu ensuite.",
  },
};

function words() {
  const language = (self.navigator.language || "en").toLowerCase();
  return language.startsWith("fr") ? WORDS.fr : WORDS.en;
}

/** `path` ("/mobile/…", "api/…") under the app's own address. */
function within(path) {
  return new URL(path.replace(/^\//, ""), self.registration.scope).href;
}

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = {};
  }
  const w = words();
  const actions = (Array.isArray(data.actions) ? data.actions : [])
    .filter((a) => a === "acknowledge" || a === "silence")
    .map((a) => ({ action: a, title: w[a] }));
  event.waitUntil(
    self.registration.showNotification(data.title || "MonitorFlow", {
      body: data.body || "",
      tag: data.tag || undefined,
      renotify: Boolean(data.tag),
      icon: within("/brand/icon-192.png"),
      badge: within("/brand/badge-96.png"),
      data: { path: data.path || "/mobile/", incident: data.incident || null },
      actions,
    }),
  );
});

async function openApp(path) {
  const url = within(path);
  const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  for (const client of windows) {
    if (new URL(client.url).origin === self.location.origin && "focus" in client) {
      await client.focus();
      return client.navigate ? client.navigate(url) : undefined;
    }
  }
  return self.clients.openWindow(url);
}

function incidentPath(id, silence) {
  return `/mobile/incident/?id=${encodeURIComponent(id)}${silence ? "&silence=1" : ""}`;
}

async function acknowledge(incident, tag) {
  try {
    const response = await fetch(within("/api/incidents/acknowledge"), {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", "X-MonitorFlow-CSRF": "1" },
      body: JSON.stringify({ incident_ids: [incident], note: "" }),
    });
    if (!response.ok) throw new Error(String(response.status));
    await self.registration.showNotification("MonitorFlow", {
      body: words().acknowledged,
      tag: tag || undefined,
      silent: true,
      icon: within("/brand/icon-192.png"),
      badge: within("/brand/badge-96.png"),
      data: { path: incidentPath(incident, false), incident: null },
    });
  } catch {
    // Signed out, or no network: the app's page says why and offers it again.
    await openApp(incidentPath(incident, false));
  }
}

self.addEventListener("notificationclick", (event) => {
  const notification = event.notification;
  const data = notification.data || {};
  notification.close();
  if (event.action === "acknowledge" && data.incident) {
    event.waitUntil(acknowledge(data.incident, notification.tag));
  } else if (event.action === "silence" && data.incident) {
    event.waitUntil(openApp(incidentPath(data.incident, true)));
  } else {
    event.waitUntil(openApp(data.path || "/mobile/"));
  }
});

function keyBytes(text) {
  const base64 = text.replace(/-/g, "+").replace(/_/g, "/");
  const binary = atob(base64 + "=".repeat((4 - (base64.length % 4)) % 4));
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

// The browser renewed the subscription: give the new one to this phone.
self.addEventListener("pushsubscriptionchange", (event) => {
  event.waitUntil(
    (async () => {
      const response = await fetch(within("/api/me/phones"), { credentials: "same-origin" });
      if (!response.ok) return;
      const list = await response.json();
      const mine = list.phones.find((p) => p.this_phone);
      if (!mine) return;
      const subscription =
        event.newSubscription ||
        (await self.registration.pushManager.subscribe({
          userVisibleOnly: true,
          applicationServerKey: keyBytes(list.push_key),
        }));
      await fetch(within(`/api/me/phones/${encodeURIComponent(mine.id)}/push`), {
        method: "PUT",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", "X-MonitorFlow-CSRF": "1" },
        body: JSON.stringify(subscription.toJSON()),
      });
    })().catch(() => {}),
  );
});
