---
title: "Comment surveiller Proxmox VE"
description: "Surveillez vos hôtes Proxmox VE : l'interface web sur le port 8006, le service de cluster, et l'espace disque, le CPU et la mémoire des VM."
slug: surveiller-proxmox-ve
lang: fr
translation: monitor-proxmox-ve
---

# Comment surveiller Proxmox VE

Un hôte Proxmox VE porte de nombreuses VM et conteneurs : s'il manque de disque ou de mémoire, ou perd son service de cluster, tous en pâtissent. MonitorFlow s'installe sur l'hôte comme sur tout serveur Linux, et surveille les services Proxmox en plus du disque, du CPU et de la mémoire.

## Pourquoi c'est important

Les problèmes commencent petits. Le stockage se remplit de snapshots et de sauvegardes jusqu'à figer les VM en écriture. La mémoire est surallouée et le noyau tue des processus. `pve-cluster` s'arrête après une coupure réseau ou un disque racine plein, et la configuration du cluster passe en lecture seule : impossible de démarrer, arrêter ou migrer une VM. `pveproxy` s'arrête et l'interface web avec lui, au pire moment.

## Ce que MonitorFlow vérifie

Proxmox VE repose sur Debian : l'agent Linux s'installe sur l'hôte, qui apparaît dans la vue d'ensemble comme tout serveur. Cherchez « proxmox » dans le catalogue des vérifications :

| Entrée du catalogue | Ce qu'elle fait | Alerte par défaut |
| --- | --- | --- |
| « Proxmox web interface running » | Le service systemd `pveproxy`, interface web et API, est actif. Métrique `service_pveproxy_active` | Critique s'il s'arrête, pendant 1 minute |
| « Proxmox web interface answers » | Le port 8006 accepte les connexions TCP. Métrique `port_8006_up` | Critique en cas d'échec, pendant 1 minute |
| « Proxmox cluster service running » | `pve-cluster`, qui détient la configuration du cluster, est actif. Métrique `service_pve_cluster_active` | Critique s'il s'arrête, pendant 1 minute |

Intégré, sans rien à ajouter :

- **Espace disque** de chaque système de fichiers monté : avertissement au-delà de 85 %, critique au-delà de 95 %, pendant 5 minutes. Un stockage non monté, comme un pool LVM-thin, n'est pas mesuré. Voir [espace disque](surveiller-espace-disque.md).
- **Activité disque :** avertissement quand un disque met plus de 200 ms en moyenne à répondre, pendant 10 minutes.
- **Mémoire :** avertissement sous 10 % disponible, critique sous 5 %, pendant 5 minutes. **CPU :** avertissement au-delà de 90 % pendant 10 minutes. Voir [CPU et mémoire](surveiller-cpu-memoire.md).
- **Hors ligne :** alerte critique quand l'hôte ne s'est pas signalé depuis 5 minutes. Voir [serveur hors ligne](alerte-serveur-hors-ligne.md).

D'autres services, comme `pvedaemon` ou `pvestatd`, se surveillent avec la vérification générique « A service is running ».

## Mise en place

1. Installez l'agent sur chaque hôte : ouvrez « Settings › Add servers » et collez la commande dans le shell de l'hôte. Proxmox n'a pas `sudo` par défaut : en root, retirez le mot `sudo` de la commande.
2. Ouvrez « Fleet » et cliquez sur l'hôte.
3. Dans l'onglet « Sensors », choisissez « Add a check ».
4. Dans « Find a check », tapez `proxmox` et choisissez « Proxmox cluster service running ». « Service » contient `pve-cluster`.
5. Laissez « Raise critical when it fails » coché.
6. Sous « Add to », choisissez « Every server of » le site qui regroupe vos hôtes, ou « This server only ».
7. Cliquez sur « Add check ».

Ajoutez de même les deux vérifications de l'interface web. Pour tous les hôtes d'un coup, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site, puis sur « Add a check ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». Pour un service, « Alert » choisit critique ou avertissement ; pour le disque ou la mémoire, réglez « Warning at » et « Critical at » en pourcentage. « For at least » fixe la durée en minutes, et « Apply to » ce serveur seul ou tous ceux du site : un hôte dont le stockage est fait pour être plein peut ainsi avoir ses propres seuils. Pour un site, utilisez « When to alert » sur sa page « Alerts and checks ».

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Send test » le vérifie. La même vérification en échec sur plusieurs hôtes part en un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: service_pve_cluster_active pve-cluster.service is 0 on pve1 (paris)
Warning: disk_used_ratio / is 0.87 on pve1 (paris)
```

Les valeurs de disque et de mémoire sont des fractions : `0.87` signifie 87 %. Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Cliquez sur « Acknowledge », puis vérifiez :

1. **Le service :** `systemctl status pve-cluster` et `journalctl -u pve-cluster -n 50`, ou l'onglet « Logs » de l'hôte.
2. **Le cluster :** `pvecm status` indique si le nœud a le quorum.
3. **Le disque racine :** un `/` plein arrête `pve-cluster` ; `df -h /` et `du -xsh /var/log /var/lib/vz` trouvent ce qui a grossi.
4. **L'interface web :** `systemctl restart pveproxy` une fois la cause réglée.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
