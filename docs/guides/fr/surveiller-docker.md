---
title: "Comment surveiller Docker"
description: "Soyez alerté quand le moteur Docker ou containerd s'arrête sur un serveur Linux, et quand un port publié par un conteneur ne répond plus."
slug: surveiller-docker
lang: fr
translation: monitor-docker
---

# Comment surveiller Docker

Quand le moteur Docker s'arrête, tous les conteneurs du serveur s'arrêtent avec lui. MonitorFlow surveille le moteur, l'environnement d'exécution en dessous, les ports publiés par vos conteneurs, ainsi que le disque et la mémoire qu'ils partagent.

## Pourquoi c'est important

Le moteur s'arrête plus souvent qu'on ne le croit : une mise à jour qui le redémarre avec une configuration refusée, une modification de `daemon.json`, un `/var/lib/docker` plein, le noyau qui le tue faute de mémoire. Les conteneurs ne redémarrent que si le moteur revient. Pendant ce temps, le serveur répond au ping et semble sain : ce sont les utilisateurs qui découvrent la panne.

## Ce que MonitorFlow vérifie

Cherchez « docker » dans le catalogue des vérifications :

| Entrée du catalogue | Ce qu'elle fait | Alerte par défaut |
| --- | --- | --- |
| « Docker running » | Le service systemd `docker` est actif. Métrique `service_docker_active`, 1 s'il est actif | Critique s'il s'arrête, pendant 1 minute |
| « containerd running » | Le service systemd `containerd`, sous Docker et Kubernetes, est actif. Métrique `service_containerd_active` | Critique s'il s'arrête, pendant 1 minute |
| « A port answers » | Un port publié par un conteneur, comme `8080`, accepte les connexions TCP. Métrique `port_8080_up` | Critique en cas d'échec, pendant 1 minute |

MonitorFlow ne liste pas les conteneurs et ne mesure pas chacun d'eux. L'utilisateur de l'agent n'a pas accès au socket Docker, volontairement : cet accès équivaut à être root. Une vérification par port publié indique que l'application répond, et c'est ce que voient les utilisateurs.

Deux vérifications intégrées comptent sur tout hôte Docker, sans configuration : l'espace disque, que remplissent images, volumes et journaux (avertissement au-delà de 85 %, critique au-delà de 95 %), et la mémoire disponible (avertissement sous 10 %, critique sous 5 %). Voir [espace disque](surveiller-espace-disque.md) et [CPU et mémoire](surveiller-cpu-memoire.md).

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans l'onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `docker` et choisissez « Docker running ». « Service » contient `docker`.
4. Laissez « Raise critical when it fails » coché.
5. Sous « Add to », choisissez « This server only », ou « Every server of » son site si tous font tourner Docker.
6. Cliquez sur « Add check ».

Ajoutez « containerd running » de la même façon. Pour chaque port publié important, ajoutez « A port answers » avec le port côté hôte, la partie gauche de `-p 8080:80`. La vérification se connecte à `127.0.0.1`, qui atteint les ports publiés sur toutes les adresses. Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site, puis sur « Add a check ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». « Alert » choisit critique ou avertissement, « For at least » le nombre de minutes que doit durer le problème (un conteneur lent à démarrer après un déploiement peut demander 2 ou 3 minutes sur son port), et « Apply to » ce serveur seul ou tous ceux du site. Pour un site, utilisez « When to alert » sur sa page « Alerts and checks ».

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Send test » le vérifie. La même vérification en échec sur plusieurs serveurs, après une mise à jour par exemple, part en un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: service_docker_active docker.service is 0 on app-1 (paris)
Critical: port_8080_up is 0 on app-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Un message `Resolved:` suit la résolution. Cliquez sur « Acknowledge », puis vérifiez :

1. **Pourquoi le moteur s'est arrêté :** `systemctl status docker` et `journalctl -u docker -n 50`, ou l'onglet « Logs » du serveur.
2. **Le disque :** `df -h /var/lib/docker`, puis `docker system df` pour les images, volumes et cache de construction.
3. **Le conteneur derrière un port :** `docker ps -a` montre s'il s'est arrêté, `docker logs <nom>` pourquoi.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
