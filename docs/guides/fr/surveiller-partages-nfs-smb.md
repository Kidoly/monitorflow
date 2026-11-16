---
title: "Comment surveiller les partages de fichiers NFS et SMB"
description: "Soyez alerté quand un serveur de fichiers ne répond plus en NFS ou SMB, que son service s'arrête, ou qu'un partage monté sur un client Linux se remplit."
slug: surveiller-partages-nfs-smb
lang: fr
translation: monitor-nfs-smb-shares
---

# Comment surveiller les partages de fichiers NFS et SMB

Quand un serveur de fichiers ne répond plus, toutes les machines qui en dépendent le ressentent en même temps. MonitorFlow surveille le serveur, les partages côté clients et leur espace libre.

## Pourquoi c'est important

Un serveur de fichiers s'arrête après une mise à jour, un changement réseau ou un service planté. Les clients ne plantent pas : leurs processus attendent le montage, une sauvegarde ne finit jamais. Le premier signalement est « le serveur est lent », pas « le partage est tombé ».

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Où | Ce qu'elle fait | Alerte à l'ajout |
| --- | --- | --- | --- |
| « NFS answers » | Linux | Le port 2049 accepte les connexions TCP. Métrique `port_2049_up` | Critique quand il échoue, pendant 1 minute |
| « File sharing answers (SMB) » | Linux, Windows | Le port 445 accepte les connexions TCP. Métrique `port_445_up` | Critique quand il échoue, pendant 1 minute |
| « A service is running » | Linux | L'unité du serveur de fichiers : `nfs-server` pour NFS, `smbd` pour Samba sous Debian et Ubuntu, `smb` sous RHEL et Rocky | Critique quand elle s'arrête, pendant 1 minute |
| « A Windows service is running » | Windows | `LanmanServer`, le service de partage de fichiers de Windows | Critique quand il s'arrête, pendant 1 minute |
| « Disk space » | Linux | Intégré : espace utilisé sur chaque point de montage, partages NFS et SMB compris. Métrique `disk_used_ratio` | Avertissement au-delà de 85 % d'occupation, critique au-delà de 95 %, pendant 5 minutes |

Une vérification de port se connecte depuis l'agent, à `127.0.0.1` par défaut : sur le serveur de fichiers, elle vérifie qu'il répond. Sur un client, saisissez le nom du serveur de fichiers dans « Address » : elle vérifie que le client le joint à travers le réseau. Si l'administrateur du client a limité les destinations des vérifications de port (`port_targets` dans `agent.toml`), le serveur de fichiers doit figurer dans cette liste.

Sur les clients Linux, un partage monté est mesuré comme un disque local, à chaque vérification, sur son point de montage. Un partage bloqué est ignoré pour ce tour au lieu de bloquer l'agent : un serveur qui ne répond plus se voit donc sur la vérification de port, pas sur l'espace disque. Windows ignore les lecteurs réseau.

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur de fichiers.
2. Dans son onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `nfs` ou `smb` et prenez « NFS answers » ou « File sharing answers (SMB) ».
4. Laissez « Raise critical when it fails » coché, choisissez « This server only » sous « Add to », puis « Add check ».
5. Ajoutez le service : cherchez `service`, prenez « A service is running » (ou « A Windows service is running ») et saisissez le nom de l'unité ou du service.
6. Sur chaque client qui monte le partage, ajoutez la même vérification de port avec le nom du serveur de fichiers dans « Address ».

L'espace disque ne demande rien : il est déjà mesuré sur chaque serveur. 

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne de la vérification et choisissez « Edit ». Pour un port ou un service, « Alert » bascule entre critique et avertissement et « For at least » fixe combien de minutes le problème doit durer. Pour l'espace d'un partage, réglez « Warning at » et « Critical at » en pourcentage. « Apply to » choisit ce seul point de montage, le serveur, ou tous les serveurs du site. Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », où la gravité s'appelle « Raise ».

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message donne la métrique, sa valeur, le serveur qui exécute la vérification et son site :

```
Critical: port_2049_up is 0 on app-1 (paris)
Warning: disk_used_ratio /mnt/backup is 0.88 on app-1 (paris)
```

Depuis un client, le serveur nommé est le client, pas le serveur de fichiers. Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. « Acknowledge » signale que vous vous en occupez. Vérifiez ensuite :

1. **Le serveur de fichiers :** est-il allumé, son service tourne-t-il ? `systemctl status nfs-server` ou `smbd`, `Get-Service LanmanServer` sous Windows.
2. **Le réseau, si seuls certains clients échouent :** depuis un client, `nc -zv <serveur de fichiers> 2049` (ou 445).
3. **L'espace :** `df -h <point de montage>` sur le client, puis ce qui a grossi sur le serveur de fichiers. Voir [Comment surveiller l'espace disque](surveiller-espace-disque.md).

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
