---
title: "Comment surveiller Docker"
description: "Suivez chaque conteneur Docker ou Podman : s'il tourne, ses redémarrages, son contrôle de santé, son processeur et sa mémoire, et soyez alerté quand le moteur ou un conteneur s'arrête."
slug: surveiller-docker
lang: fr
translation: monitor-docker
---

# Comment surveiller Docker

Quand le moteur Docker s'arrête, tous les conteneurs du serveur s'arrêtent avec lui, et un seul conteneur peut planter en boucle pendant que le serveur semble aller bien. MonitorFlow surveille chaque conteneur, le moteur et l'environnement d'exécution en dessous, les ports publiés par vos conteneurs, ainsi que le disque et la mémoire qu'ils partagent.

## Pourquoi c'est important

Le moteur s'arrête plus souvent qu'on ne le croit : une mise à jour qui le redémarre avec une configuration refusée, une modification de `daemon.json`, un `/var/lib/docker` plein, le noyau qui le tue faute de mémoire. Les conteneurs ne redémarrent que si le moteur revient. Un conteneur peut aussi s'arrêter seul, redémarrer toutes les quelques secondes, échouer à son contrôle de santé ou atteindre sa limite de mémoire. Pendant ce temps, le serveur répond au ping et semble sain : ce sont les utilisateurs qui découvrent la panne.

## Ce que MonitorFlow vérifie

### Le contrôle des conteneurs Docker

L'entrée « Conteneurs Docker » du catalogue (un « Contrôle d’application ») lit l'API du moteur local chaque minute, sur les serveurs Linux avec l'agent. Pour chaque conteneur surveillé, il lit :

- s'il tourne, avec son image et son état ;
- ses redémarrages, au total et depuis le contrôle précédent ;
- son contrôle de santé, s'il en a un : 0 sain, 1 en démarrage, 2 en échec ;
- sa part du temps processeur de la machine, et sa mémoire par rapport à sa limite.

Il compte aussi les conteneurs surveillés qui tournent et ceux qui ne tournent pas, et indique si le moteur répond.

**Quels conteneurs sont surveillés :** ceux qui tournent, et ceux arrêtés dont la politique de redémarrage est `always` ou `unless-stopped`, car ils sont censés tourner. Les conteneurs ponctuels qui se sont terminés sont laissés de côté. « Conteneurs » restreint la liste avec une expression régulière sur leurs noms.

Les métriques portent le nom du contrôle : `monitorflow_docker_up`, `monitorflow_docker_container_running`, etc. (avec un autre nom de contrôle, ce nom remplace `docker`). Les métriques de chaque conteneur portent son nom.

Avec « Ajouter les alertes recommandées » activé, ces alertes l'accompagnent :

| Métrique | Avertissement | Critique | Pendant |
| --- | --- | --- | --- |
| `monitorflow_docker_up` (le moteur répond) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_docker_container_running` (par conteneur) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_docker_container_restarts` (par conteneur, depuis le contrôle précédent) | Au-delà de 0 | Aucun | Tout de suite |
| `monitorflow_docker_container_health` (par conteneur) | Aucun | 2 ou plus (en échec) | 2 minutes |
| `monitorflow_docker_container_memory_ratio` (par conteneur) | Au-delà de 0,9 (90 %) | Aucun | 5 minutes |

### Vérifications de service et de port

Cherchez « docker » dans le catalogue des vérifications pour les vérifications plus simples :

| Entrée du catalogue | Ce qu'elle fait | Alerte par défaut |
| --- | --- | --- |
| « Docker running » | Le service systemd `docker` est actif. Métrique `service_docker_active`, 1 s'il est actif | Critique s'il s'arrête, pendant 1 minute |
| « containerd running » | Le service systemd `containerd`, sous Docker et Kubernetes, est actif. Métrique `service_containerd_active` | Critique s'il s'arrête, pendant 1 minute |
| « A port answers » | Un port publié par un conteneur, comme `8080`, accepte les connexions TCP. Métrique `port_8080_up` | Critique en cas d'échec, pendant 1 minute |

Celles-ci n'ont besoin d'aucun accès au moteur. Une vérification par port publié indique que l'application répond, et c'est ce que voient les utilisateurs.

Deux vérifications intégrées comptent sur tout hôte Docker, sans configuration : l'espace disque, que remplissent images, volumes et journaux (avertissement au-delà de 85 %, critique au-delà de 95 %), et la mémoire disponible (avertissement sous 10 %, critique sous 5 %). Voir [espace disque](surveiller-espace-disque.md) et [CPU et mémoire](surveiller-cpu-memoire.md).

## Laisser l'agent lire le moteur

L'utilisateur de l'agent, `monitorflow`, n'a pas accès au socket Docker, volontairement : qui peut utiliser ce socket peut lancer un conteneur qui monte tout le disque, cet accès équivaut donc à être root sur la machine. Ne l'accordez que si vous l'acceptez. Le contrôle, lui, ne fait que lire (requêtes `GET`) : il ne démarre, n'arrête ni ne modifie jamais un conteneur.

Pour l'accorder, ajoutez l'utilisateur de l'agent au groupe `docker` et redémarrez l'agent :

```sh
sudo usermod -aG docker monitorflow
sudo systemctl restart monitorflow-agent
```

D'ici là, le contrôle indique `docker_up` à 0 avec la raison `permission`, et son erreur dit quoi lancer.

**Podman :** choisissez « Podman » dans « Moteur ». Le contrôle lit le socket root, `/run/podman/podman.sock` : activez-le avec `sudo systemctl enable --now podman.socket`. Ce socket appartient à root ; pour que l'utilisateur de l'agent puisse l'ouvrir, donnez-lui un groupe, par exemple avec `sudo systemctl edit podman.socket` et ces lignes, puis `sudo systemctl restart podman.socket` :

```ini
[Socket]
SocketGroup=monitorflow
SocketMode=0660
```

Le même avertissement vaut : l'accès au socket Podman équivaut à être root.

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans l'onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `docker` et choisissez « Conteneurs Docker », le « Contrôle d’application ».
4. Laissez « Conteneurs » vide pour surveiller tous les conteneurs censés tourner, ou saisissez une expression régulière comme `^web-` pour ne surveiller que ceux dont le nom commence par `web-`.
5. Laissez « Ajouter les alertes recommandées » activé.
6. Sous « Add to », choisissez « This server only », ou « Every server of » son site si tous font tourner Docker, puis cliquez sur « Add check ».

« Plus d’options » contient « Moteur » : « Docker » (par défaut, `/var/run/docker.sock`) ou « Podman ». Le contrôle tourne toujours sur l'agent du serveur lui-même, jamais depuis le serveur central ou un relais, car le socket est local.

Pour les vérifications plus simples, ajoutez « Docker running » (« Service » contient `docker`) et « containerd running » de la même façon, avec « Raise critical when it fails » coché. Pour chaque port publié important, ajoutez « A port answers » avec le port côté hôte, la partie gauche de `-p 8080:80`. La vérification se connecte à `127.0.0.1`, qui atteint les ports publiés sur toutes les adresses. Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site, puis sur « Add a check ».

### Par l'API

L'API publique ajoute le même contrôle : `POST /api/v1/servers/{id}/sensor_configs` avec le type, ses paramètres, et `default_alerts` pour ajouter les alertes recommandées. `GET /api/v1/check-templates` liste les métriques et alertes de chaque type.

```json
{
  "kind": "docker",
  "interval_ms": 60000,
  "params": { "engine": "docker", "containers": "^web-" },
  "default_alerts": true
}
```

## Modifier l'alerte

Dans l'onglet « Sensors », les alertes recommandées sont des alertes ordinaires, une ligne chacune sous « When to alert ». Ouvrez le menu d'une ligne et choisissez « Edit » : « Alert » choisit critique ou avertissement, « For at least » le nombre de minutes que doit durer le problème (un conteneur lent à démarrer après un déploiement peut demander 3 ou 5 minutes sur `container_running`), et « Apply to » ce serveur seul ou tous ceux du site. L'alerte de redémarrage se déclenche tout de suite exprès : un conteneur qui redémarre seul a planté.

Les vérifications de service et de port se règlent de la même façon : ouvrez le menu de la ligne et choisissez « Edit ». Pour un site, utilisez « When to alert » sur sa page « Alerts and checks ».

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Send test » le vérifie. La même vérification en échec sur plusieurs serveurs, après une mise à jour par exemple, part en un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: docker_up is 0 on app-1 (paris)
Critical: docker_container_running web-1 is 0 on app-1 (paris)
Warning: docker_container_restarts web-1 is 1 on app-1 (paris)
Critical: service_docker_active docker.service is 0 on app-1 (paris)
Critical: port_8080_up is 0 on app-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Un message `Resolved:` suit la résolution.

Quand `docker_up` vaut 0, son étiquette `reason` dit pourquoi, et son étiquette `error` donne le détail :

| `reason` | Que vérifier |
| --- | --- |
| `connect` | Pas de socket du moteur : le moteur est arrêté, n'est pas installé, ou « Moteur » ne désigne pas le bon (pour Podman, `podman.socket` n'est pas activé) |
| `permission` | L'utilisateur de l'agent ne peut pas ouvrir le socket : voir « Laisser l'agent lire le moteur » plus haut |
| `timeout` | Le moteur n'a pas répondu à temps : il est peut-être bloqué ou surchargé |
| `protocol` | Le moteur a répondu par une erreur |

Cliquez sur « Acknowledge », puis vérifiez :

1. **Pourquoi le moteur s'est arrêté :** `systemctl status docker` et `journalctl -u docker -n 50`, ou l'onglet « Logs » du serveur.
2. **Le disque :** `df -h /var/lib/docker`, puis `docker system df` pour les images, volumes et cache de construction.
3. **Un conteneur arrêté ou qui redémarre :** `docker ps -a` montre son état, `docker logs <nom>` pourquoi, et `docker inspect <nom>` son code de sortie et s'il a été tué faute de mémoire (`OOMKilled`).
4. **Un conteneur en échec de santé :** `docker inspect --format '{{json .State.Health}}' <nom>` montre les derniers résultats de son contrôle de santé.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
