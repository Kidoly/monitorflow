---
title: "Comment surveiller Kubernetes"
description: "Suivez un cluster Kubernetes par son API avec un jeton en lecture seule : les nœuds et leurs conditions, les pods qui ne tournent pas ou redémarrent, les déploiements sans assez de réplicas prêts et les volume claims en attente."
slug: surveiller-kubernetes
lang: fr
translation: monitor-kubernetes
---

# Comment surveiller Kubernetes

Kubernetes relance ce qui plante, et du même coup le cache : un pod peut redémarrer toutes les quelques minutes pendant des jours, un déploiement tourner sur un réplica sur trois, un nœud manquer de disque pendant que les applications répondent encore. MonitorFlow lit l’API du cluster avec un jeton en lecture seule et vous prévient quand cela arrive.

## Pourquoi c’est important

Le cluster réessaie sans cesse, alors les problèmes durent au lieu d’échouer bruyamment. Une nouvelle image qui ne démarre pas laisse tourner les anciens pods jusqu’au prochain drain de nœud. Un nœud à court de mémoire évince les pods l’un après l’autre. Un volume claim attend une storage class qui n’existe pas, et son stateful set ne démarre jamais. Les utilisateurs ne le voient qu’au départ du dernier réplica.

## Ce que MonitorFlow vérifie

Le cluster est un **équipement** dans MonitorFlow, et son contrôle « Cluster Kubernetes » lit son API depuis le serveur central ou un relais du site, aussi souvent que le dit « Fréquence du contrôle ». Aucun agent n’entre dans le cluster, et chaque requête est une lecture : le contrôle n’y change rien.

| Quoi | Métriques | Une par |
| --- | --- | --- |
| L’API répond et accepte le jeton | `up` (étiquette `version`), `response_seconds` | |
| Nœuds | `nodes`, `nodes_not_ready`, `node_ready` (étiquettes `roles` et `kubelet_version`), `node_memory_pressure`, `node_disk_pressure`, `node_pid_pressure`, `node_network_unavailable`, `node_unschedulable` | nœud |
| Pods | `pods`, `pods_not_running`, `namespace_pods`, `namespace_pods_not_running` (l’étiquette `pods` les nomme et dit pourquoi), `namespace_pod_restarts` (redémarrages de conteneurs depuis le dernier contrôle) | namespace |
| Deployments, stateful sets et daemon sets | `workload_replicas_desired`, `workload_replicas_ready`, `workload_ready_ratio`, `workloads_short` | charge, nommée `namespace/deployment/nom` |
| Persistent volume claims | `claims_pending`, `namespace_claims_pending`, `namespace_claims_lost` (l’étiquette `claims` les nomme) | namespace |

**Un pod compte comme ne tournant pas** quand il est en attente, en échec ou inconnu, ou qu’il tourne sans être prêt, comme un pod en `CrashLoopBackOff`. Les pods terminés (`Succeeded`), les pods en échec d’un job et ceux en cours de suppression sont laissés de côté. « Namespaces » limite les pods, les charges et les claims aux namespaces qui correspondent à une expression régulière ; les nœuds sont toujours ceux de tout le cluster.

Les métriques portent le nom du contrôle : `monitorflow_kubernetes_node_ready`, `monitorflow_kubernetes_namespace_pods_not_running`, et ainsi de suite (avec un autre nom de contrôle, ce nom remplace `kubernetes`).

Avec « Ajouter les alertes recommandées » activé, ces alertes l’accompagnent :

| Métrique | Avertissement | Critique | Pendant |
| --- | --- | --- | --- |
| `monitorflow_kubernetes_up` (l’API répond) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_kubernetes_node_ready` (par nœud) | Aucun | Sous 1 | 5 minutes |
| `monitorflow_kubernetes_node_memory_pressure`, `_disk_pressure`, `_pid_pressure` (par nœud) | 1 | Aucun | 5 minutes |
| `monitorflow_kubernetes_node_network_unavailable` (par nœud) | Aucun | 1 | 5 minutes |
| `monitorflow_kubernetes_namespace_pods_not_running` (par namespace) | Plus de 0 | Aucun | 10 minutes |
| `monitorflow_kubernetes_namespace_pod_restarts` (par namespace, depuis le dernier contrôle) | Plus de 0 | Aucun | Tout de suite |
| `monitorflow_kubernetes_workload_ready_ratio` (par charge) | Sous 1 | Sous 0,5 | 10 minutes |
| `monitorflow_kubernetes_namespace_claims_pending` (par namespace) | Plus de 0 | Aucun | 10 minutes |
| `monitorflow_kubernetes_namespace_claims_lost` (par namespace) | Aucun | Plus de 0 | Tout de suite |

Une alerte couvre tous les nœuds, namespaces ou charges : un pod qui plante dans `shop` est un seul incident qui nomme `shop` et le pod.

## Donner à MonitorFlow un compte en lecture seule

Le contrôle a besoin d’un compte de service qui peut seulement **lire et lister** (`get`, `list`) les nœuds, les pods, les persistent volume claims, les deployments, les stateful sets et les daemon sets. Il ne peut ni lire les secrets, ni ouvrir un shell dans un pod, ni rien modifier. Enregistrez ceci sous `monitorflow-reader.yaml` (il est aussi dans le dépôt, sous `deploy/kubernetes/`) :

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

Appliquez-le, puis lisez le jeton et la CA du cluster :

```sh
kubectl apply -f monitorflow-reader.yaml
kubectl -n monitorflow get secret monitorflow-reader-token -o jsonpath='{.data.token}' | base64 -d; echo
kubectl -n monitorflow get secret monitorflow-reader-token -o jsonpath='{.data.ca\.crt}'; echo
```

La première ligne affichée est le jeton, la seconde la CA en base64, que MonitorFlow prend telle quelle. Vous pouvez vérifier ce que le compte peut faire :

```sh
kubectl auth can-i list pods -A --as=system:serviceaccount:monitorflow:monitorflow-reader      # yes
kubectl auth can-i get secrets -A --as=system:serviceaccount:monitorflow:monitorflow-reader    # no
```

Le jeton de ce secret n’expire pas, et supprimer le secret le révoque. Pour un jeton qui expire, créez-en un avec `kubectl -n monitorflow create token monitorflow-reader --duration=8760h` et saisissez le suivant sur le contrôle avant son expiration : sinon le contrôle indique `up` à 0 avec la raison `login`.

**Le réseau :** le serveur central ou le relais qui vérifie le cluster doit joindre son serveur d’API, en général sur le port 6443. Un cluster managé (EKS, AKS, GKE) dont l’API est privée se vérifie depuis un relais dans son réseau.

## Mise en place

1. Dans le parc, choisissez « Ajouter un équipement ». Donnez-lui un nom comme `k8s-prod` et l’adresse du serveur d’API : l’hôte de `server:` dans votre kubeconfig, sans `https://` ni le port.
2. Sur la page de l’équipement, ouvrez l’onglet « Capteurs », choisissez « Ajouter un contrôle », puis « Cluster Kubernetes ».
3. Laissez « Adresse à vérifier » vide pour utiliser l’adresse de l’équipement, et « Port » à `6443`, sauf si votre serveur d’API écoute ailleurs (443 sur la plupart des clusters managés).
4. Collez le jeton dans « Jeton du compte de service ».
5. Collez la CA dans « CA du cluster » : le base64 affiché plus haut, le `certificate-authority-data` de votre kubeconfig, ou la CA en PEM. Ne la laissez vide que si le certificat de l’API est signé par une autorité à laquelle le serveur central ou le relais fait déjà confiance, comme pour certains clusters managés.
6. Sous « Plus d’options », « Namespaces » prend une expression régulière comme `^(shop|billing)$` pour ne surveiller que ceux-là.
7. Laissez « Ajouter les alertes recommandées » activé et cliquez sur « Ajouter le contrôle ».

Le jeton et la CA sont gardés scellés ensemble sur le serveur, jamais réaffichés, jamais écrits dans le journal d’audit, et donnés seulement au serveur central ou au relais qui exécute le contrôle, par son lien chiffré. Le certificat de l’API est toujours vérifié : il n’y a aucun moyen de passer outre, car le jeton partirait alors chez quiconque répond.

### Par l’API

L’API publique ajoute le même contrôle : `POST /api/v1/servers/{id}/sensor_configs` sur l’équipement, avec `probe` à `central` ou à l’identifiant du relais, et `default_alerts` pour ajouter les alertes recommandées. `GET /api/v1/check-templates` liste chaque métrique et chaque alerte.

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

La réponse indique `"secret_set": true` et ne renvoie jamais le jeton ni la CA. Une mise à jour sans `token` garde les deux ; une mise à jour avec `token` remplace les deux, donnez donc aussi `ca_certificate`. Un jeton gardé ne suit jamais le contrôle vers une nouvelle adresse ou un nouveau port : si vous les changez, redonnez le jeton.

## Modifier l’alerte

Dans l’onglet « Capteurs » de l’équipement, les alertes recommandées sont des alertes ordinaires, une ligne chacune sous « Quand alerter ». Ouvrez le menu d’une ligne et choisissez « Modifier » : « Alerte » choisit critique ou avertissement, « Pendant au moins » le nombre de minutes que doit durer le problème, et « Appliquer à » cet équipement seul ou tous ceux du site. L’alerte de redémarrage se déclenche tout de suite exprès : un conteneur qui redémarre seul a planté. Dans un cluster où certains jobs redémarrent par conception, montez-la à quelques redémarrages, ou limitez « Namespaces ».

## Où part l’alerte

Chaque alerte s’affiche dans « Supervision › Incidents » et dans le parc. Pour être prévenu, ajoutez un canal dans « Paramètres › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Envoyer un test » le vérifie. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « Astreinte ».

## Quand l’alerte se déclenche

```
Critical: kubernetes_node_ready worker-2 is 0 on k8s-prod (paris)
Warning: kubernetes_namespace_pods_not_running shop is 1 on k8s-prod (paris)
Warning: kubernetes_workload_ready_ratio shop/deployment/web is 0.67 on k8s-prod (paris)
Warning: kubernetes_namespace_claims_pending shop is 1 on k8s-prod (paris)
```

Quand `kubernetes_up` vaut 0, son étiquette `reason` dit pourquoi, et son étiquette `error` donne le détail :

| `reason` | Que vérifier |
| --- | --- |
| `connect` | Le serveur central ou le relais ne joint pas le serveur d’API : adresse, port, pare-feu, ou API privée |
| `tls` | Le certificat de l’API n’est pas reconnu : saisissez la CA du cluster sur le contrôle, ou vérifiez que l’adresse est l’une de celles que nomme le certificat |
| `login` | Pas de jeton, ou l’API l’a refusé (401) : il a expiré, a été révoqué, ou appartient à un autre cluster |
| `permission` | Le compte n’a pas le droit de lire quelque chose (403) : appliquez le ClusterRole et sa liaison ci-dessus |
| `timeout` | L’API n’a pas répondu à temps |
| `protocol` | L’API a répondu par une erreur, ou liste plus de 10 000 objets d’un type : limitez « Namespaces » |

Cliquez sur « Prendre en charge », puis vérifiez :

1. **Un nœud pas prêt :** `kubectl describe node <nom>` montre ses conditions et ses événements ; sur le nœud, `journalctl -u kubelet`.
2. **Un pod qui ne tourne pas :** l’étiquette `pods` de l’incident le nomme et dit pourquoi (`CrashLoopBackOff`, `ImagePullBackOff`, `Pending`). `kubectl -n <namespace> describe pod <nom>` et `kubectl -n <namespace> logs <nom> --previous` en disent plus.
3. **Une charge sans assez de réplicas :** `kubectl -n <namespace> rollout status deployment/<nom>`, puis ses pods comme ci-dessus.
4. **Un claim en attente :** `kubectl -n <namespace> describe pvc <nom>` montre si sa storage class existe et ce qu’a répondu le provisionneur.

## Venir d’un autre outil

L’import transforme ce qui lit un cluster en ce contrôle, qui attend son jeton et sa CA :

- **PRTG** n’a pas de capteur Kubernetes : les capteurs REST Custom et HTTP Advanced dont l’URL est l’API du cluster (`/api/v1/...`, `/apis/...`, ou `/healthz`, `/livez`, `/readyz` sur le port 6443), et les capteurs personnalisés nommés d’après Kubernetes, deviennent le contrôle Kubernetes de leur équipement. Voir [migrer depuis PRTG](migrer-depuis-prtg.md).
- Les modèles **Zabbix** « Kubernetes cluster state by HTTP », « Kubernetes nodes by HTTP » et leurs voisins deviennent un seul contrôle Kubernetes, à l’adresse de `{$KUBE.API.URL}`. Le jeton, une macro secrète, n’est jamais exporté.
- Les jobs **Prometheus** qui collectent le serveur d’API (`kubernetes-apiservers`, ou des cibles sur le port 6443) deviennent le contrôle de cette adresse.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Paramètres › Ajouter des serveurs » dans l’interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l’agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
