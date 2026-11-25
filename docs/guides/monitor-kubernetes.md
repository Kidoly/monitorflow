---
title: "How to monitor Kubernetes"
description: "Watch a Kubernetes cluster through its API with a read-only token: nodes and their conditions, pods that do not run or restart, deployments short of ready replicas and volume claims left pending."
slug: monitor-kubernetes
lang: en
translation: surveiller-kubernetes
---

# How to monitor Kubernetes

Kubernetes restarts what crashes, which also hides it: a pod can restart every few minutes for days, a deployment can run on one replica out of three, a node can run out of disk space while the applications still answer. MonitorFlow reads the cluster's API with a read-only token and tells you when that happens.

## Why it matters

The cluster keeps trying, so problems last instead of failing loudly. A new image that does not start leaves the old pods running until the next node drain. A node under memory pressure evicts pods one after the other. A volume claim waits for a storage class that does not exist, and its stateful set never starts. Users only notice when the last replica goes.

## What MonitorFlow checks

The cluster is a **device** in MonitorFlow, and its **"Kubernetes cluster"** check reads its API from the central server or a relay of the site, as often as **"How often to check"** says. No agent goes into the cluster, and every request is a read: the check changes nothing there.

| What | Metrics | One per |
| --- | --- | --- |
| The API answers and takes the token | `up` (label `version`), `response_seconds` | |
| Nodes | `nodes`, `nodes_not_ready`, `node_ready` (labels `roles` and `kubelet_version`), `node_memory_pressure`, `node_disk_pressure`, `node_pid_pressure`, `node_network_unavailable`, `node_unschedulable` | node |
| Pods | `pods`, `pods_not_running`, `namespace_pods`, `namespace_pods_not_running` (label `pods` names them and says why), `namespace_pod_restarts` (container restarts since the last check) | namespace |
| Deployments, stateful sets and daemon sets | `workload_replicas_desired`, `workload_replicas_ready`, `workload_ready_ratio`, `workloads_short` | workload, named `namespace/deployment/name` |
| Persistent volume claims | `claims_pending`, `namespace_claims_pending`, `namespace_claims_lost` (label `claims` names them) | namespace |

**A pod counts as not running** when it is pending, failed or unknown, or running but not ready, as a pod in `CrashLoopBackOff` is. Pods that finished (`Succeeded`), failed pods of a job and pods being deleted are left out. **"Namespaces"** narrows pods, workloads and claims to the namespaces matching a regular expression; nodes are always the whole cluster's.

Metrics are named after the check: `monitorflow_kubernetes_node_ready`, `monitorflow_kubernetes_namespace_pods_not_running`, and so on (with another check name, that name replaces `kubernetes`).

With **"Add the recommended alerts"** on, these alerts come with it:

| Metric | Warning | Critical | For |
| --- | --- | --- | --- |
| `monitorflow_kubernetes_up` (the API answers) | None | Under 1 | 2 minutes |
| `monitorflow_kubernetes_node_ready` (per node) | None | Under 1 | 5 minutes |
| `monitorflow_kubernetes_node_memory_pressure`, `_disk_pressure`, `_pid_pressure` (per node) | 1 | None | 5 minutes |
| `monitorflow_kubernetes_node_network_unavailable` (per node) | None | 1 | 5 minutes |
| `monitorflow_kubernetes_namespace_pods_not_running` (per namespace) | Over 0 | None | 10 minutes |
| `monitorflow_kubernetes_namespace_pod_restarts` (per namespace, since the last check) | Over 0 | None | At once |
| `monitorflow_kubernetes_workload_ready_ratio` (per workload) | Under 1 | Under 0.5 | 10 minutes |
| `monitorflow_kubernetes_namespace_claims_pending` (per namespace) | Over 0 | None | 10 minutes |
| `monitorflow_kubernetes_namespace_claims_lost` (per namespace) | None | Over 0 | At once |

One alert covers every node, namespace or workload: a crashing pod in `shop` is one incident that names `shop` and the pod.

## Give MonitorFlow a read-only account

The check needs a service account that may only **get and list** nodes, pods, persistent volume claims, deployments, stateful sets and daemon sets. It cannot read secrets, open a shell in a pod or change anything. Save this as `monitorflow-reader.yaml` (it is also in the repository, under `deploy/kubernetes/`):

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: monitorflow
---
apiVersion: v1
kind: ServiceAccount
metadata:
  name: monitorflow-reader
  namespace: monitorflow
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: monitorflow-reader
rules:
  - apiGroups: [""]
    resources: ["nodes", "pods", "persistentvolumeclaims"]
    verbs: ["get", "list"]
  - apiGroups: ["apps"]
    resources: ["deployments", "statefulsets", "daemonsets"]
    verbs: ["get", "list"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: monitorflow-reader
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: ClusterRole
  name: monitorflow-reader
subjects:
  - kind: ServiceAccount
    name: monitorflow-reader
    namespace: monitorflow
---
apiVersion: v1
kind: Secret
metadata:
  name: monitorflow-reader-token
  namespace: monitorflow
  annotations:
    kubernetes.io/service-account.name: monitorflow-reader
type: kubernetes.io/service-account-token
```

Apply it, then read the token and the cluster's CA:

```sh
kubectl apply -f monitorflow-reader.yaml
kubectl -n monitorflow get secret monitorflow-reader-token -o jsonpath='{.data.token}' | base64 -d; echo
kubectl -n monitorflow get secret monitorflow-reader-token -o jsonpath='{.data.ca\.crt}'; echo
```

The first line printed is the token, the second the CA as base64, which MonitorFlow takes as it is. You can check what the account may do:

```sh
kubectl auth can-i list pods -A --as=system:serviceaccount:monitorflow:monitorflow-reader      # yes
kubectl auth can-i get secrets -A --as=system:serviceaccount:monitorflow:monitorflow-reader    # no
```

That secret's token does not expire, and deleting the secret revokes it. To use a token that expires instead, make one with `kubectl -n monitorflow create token monitorflow-reader --duration=8760h` and enter a new one on the check before it expires: the check then reports `up` 0 with the reason `login`.

**The network:** the central server or the relay that checks the cluster must reach its API server, usually on port 6443. A managed cluster (EKS, AKS, GKE) whose API is private is checked from a relay inside its network.

## Set it up

1. In the fleet, choose **"Add a device"**. Give it a name like `k8s-prod` and the address of the API server: the host of `server:` in your kubeconfig, without `https://` and the port.
2. On the device's page, open the **"Sensors"** tab, choose **"Add a check"**, then **"Kubernetes cluster"**.
3. Leave **"Address to check"** empty to use the device's address, and **"Port"** at `6443` unless your API server listens elsewhere (443 on most managed clusters).
4. Paste the token in **"Service account token"**.
5. Paste the CA in **"Cluster CA"**: the base64 printed above, the `certificate-authority-data` of your kubeconfig, or the CA in PEM. Leave it empty only when the API's certificate is signed by an authority the central server or relay already trusts, as some managed clusters' are.
6. Under **"More options"**, **"Namespaces"** takes a regular expression like `^(shop|billing)$` to watch only those.
7. Leave **"Add the recommended alerts"** on and click **"Add check"**.

The token and the CA are kept sealed together on the server, never shown again, never written in the audit log, and given only to the central server or relay that runs the check, over its encrypted link. The API's certificate is always verified: there is no way to skip it, as the token would then go to whoever answers.

### Through the API

The public API adds the same check: `POST /api/v1/servers/{id}/sensor_configs` on the device, with `probe` set to `central` or the relay's ID, and `default_alerts` to add the recommended alerts. `GET /api/v1/check-templates` lists every metric and alert.

```json
{
  "kind": "kubernetes",
  "interval_ms": 300000,
  "probe": "central",
  "params": {
    "port": "6443",
    "token": "eyJhbGciOiJSUzI1NiIs...",
    "ca_certificate": "LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0t...",
    "namespaces": "^(shop|billing)$"
  },
  "default_alerts": true
}
```

The answer says `"secret_set": true` and never returns the token or the CA. An update without `token` keeps both; one with `token` replaces both, so give `ca_certificate` again with it. A kept token never follows the check to a new address or port: change those and give the token again.

## Change the alert

On the device's **"Sensors"** tab, the recommended alerts are ordinary alerts, one line each under **"When to alert"**. Open a line's menu and choose **"Edit"**: **"Alert"** chooses critical or warning, **"For at least"** how many minutes the problem must last, and **"Apply to"** this device only or every device of the site. The restart alert fires at once on purpose: a container that restarts by itself has crashed. In a cluster where some jobs restart by design, raise it to a few restarts, or narrow **"Namespaces"**.

## Where the alert goes

Every alert shows in **Monitoring › Incidents** and on the fleet. To be told, add a channel under **Settings › Notifications**: email, webhook, Slack, Teams or Discord, for everything, one customer or one site; **"Send test"** checks it. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

```
Critical: kubernetes_node_ready worker-2 is 0 on k8s-prod (paris)
Warning: kubernetes_namespace_pods_not_running shop is 1 on k8s-prod (paris)
Warning: kubernetes_workload_ready_ratio shop/deployment/web is 0.67 on k8s-prod (paris)
Warning: kubernetes_namespace_claims_pending shop is 1 on k8s-prod (paris)
```

When `kubernetes_up` is 0, its `reason` label says why, and its `error` label gives the details:

| `reason` | What to check |
| --- | --- |
| `connect` | The central server or relay cannot reach the API server: address, port, a firewall, or a private API |
| `tls` | The API's certificate is not trusted: enter the cluster's CA on the check, or check that the address is one the certificate names |
| `login` | No token, or the API refused it (401): it expired, was revoked, or is for another cluster |
| `permission` | The account may not read something (403): apply the ClusterRole and binding above |
| `timeout` | The API did not answer in time |
| `protocol` | The API answered with an error, or lists more than 10,000 objects of a kind: narrow **"Namespaces"** |

Click **"Acknowledge"**, then check:

1. **A node not ready:** `kubectl describe node <name>` shows its conditions and events; on the node, `journalctl -u kubelet`.
2. **A pod not running:** the incident's `pods` label names it and says why (`CrashLoopBackOff`, `ImagePullBackOff`, `Pending`). `kubectl -n <namespace> describe pod <name>` and `kubectl -n <namespace> logs <name> --previous` say more.
3. **A workload short of replicas:** `kubectl -n <namespace> rollout status deployment/<name>`, and its pods as above.
4. **A pending claim:** `kubectl -n <namespace> describe pvc <name>` shows whether its storage class exists and what the provisioner said.

## Moving from another tool

The importer turns what reads a cluster into this check, waiting for its token and CA:

- **PRTG** has no Kubernetes sensor of its own: REST Custom and HTTP Advanced sensors whose URL is the cluster's API (`/api/v1/...`, `/apis/...`, or `/healthz`, `/livez`, `/readyz` on port 6443), and custom sensors named after Kubernetes, become the Kubernetes check of their device. See [moving from PRTG](move-from-prtg.md).
- **Zabbix**'s templates "Kubernetes cluster state by HTTP", "Kubernetes nodes by HTTP" and their siblings become one Kubernetes check, at the address of `{$KUBE.API.URL}`. The token, a secret macro, is never exported.
- **Prometheus** jobs scraping the API server (`kubernetes-apiservers`, or targets on port 6443) become the check of that address.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
