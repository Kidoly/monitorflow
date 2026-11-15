---
title: "Comment surveiller Apache"
description: "Soyez alerté quand le serveur web Apache s'arrête ou que les ports 80 et 443 ne répondent plus, sous Debian, Ubuntu, RHEL et Rocky."
slug: surveiller-apache
lang: fr
translation: monitor-apache
---

# Comment surveiller Apache

Le serveur HTTP Apache fait encore tourner bien des sites, intranets et applications PHP. Quand il s'arrête, tous les sites du serveur s'arrêtent avec lui. MonitorFlow surveille le service Apache et ses ports, et vous prévient en une ou deux minutes.

## Pourquoi c'est important

Apache s'arrête pour des raisons banales : un module ou un hôte virtuel en erreur après une modification, un certificat déplacé, tous ses processus bloqués derrière une application lente, un disque plein, une mise à jour. Un Apache arrêté ne fait pas de bruit : le premier signalement vient d'un utilisateur.

## Ce que MonitorFlow vérifie

Cherchez « apache » dans le catalogue des vérifications :

| Entrée du catalogue | Ce qu'elle fait | Alerte par défaut |
| --- | --- | --- |
| « Apache running » | Le service systemd est actif : `apache2` sous Debian et Ubuntu, `httpd` sous RHEL et Rocky. Métrique `service_apache2_active` ou `service_httpd_active`, 1 s'il est actif | Critique s'il s'arrête, pendant 1 minute |
| « Web server answers (HTTP) » | Le port 80 accepte les connexions TCP. Métrique `port_80_up` | Critique en cas d'échec, pendant 1 minute |
| « Web server answers (HTTPS) » | Le port 443 accepte les connexions TCP. Métrique `port_443_up` | Critique en cas d'échec, pendant 1 minute |

Un service absent compte comme arrêté, ce qui signale aussi un mauvais nom d'unité. Une vérification de port ouvre seulement une connexion TCP : elle prouve qu'Apache accepte les connexions, pas que les pages s'affichent ; pour cela, voir [Comment surveiller un site web en HTTPS](surveiller-site-web-https.md).

Pour voir venir les erreurs, ajoutez « A log file gets a line » sur `/var/log/apache2/error.log` ou `/var/log/httpd/error_log`, avec un motif comme `\[[a-z0-9_]*:(crit|alert|emerg)\]`. Par défaut, l'utilisateur de l'agent, `monitorflow`, ne peut pas lire ce dossier. Voir [Comment surveiller des fichiers journaux](surveiller-fichiers-journaux.md).

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans l'onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `apache` et choisissez « Apache running ».
4. « Service » contient `apache2`. Sous RHEL, Rocky et la famille Red Hat, remplacez-le par `httpd`.
5. Laissez « Raise critical when it fails » coché.
6. Sous « Add to », choisissez « This server only », ou « Every server of » son site si tous ont le même système.
7. Cliquez sur « Add check ».

Ajoutez les deux ports de la même façon. Une vérification de port se connecte à `127.0.0.1` ; si Apache écoute ailleurs, indiquez l'adresse dans « Address ». Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site, puis sur « Add a check ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». « Alert » choisit critique ou avertissement, « For at least » le nombre de minutes que doit durer le problème (augmentez-le si une tâche nocturne redémarre Apache, ou `0` pour alerter dès la première mauvaise valeur), et « Apply to » ce serveur seul ou tous ceux du site. Pour un site, utilisez « When to alert » sur sa page « Alerts and checks ».

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Send test » le vérifie. La même vérification en échec sur plusieurs serveurs part en un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: service_apache2_active apache2.service is 0 on web-2 (lyon)
Critical: port_80_up is 0 on web-2 (lyon)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Un message `Resolved:` suit la résolution. Cliquez sur « Acknowledge », puis vérifiez :

1. **Pourquoi il s'est arrêté :** `systemctl status apache2` (ou `httpd`) et `journalctl -u apache2 -n 50`, ou l'onglet « Logs » du serveur.
2. **La configuration :** `sudo apachectl configtest` indique le fichier et la ligne fautifs.
3. **Le journal d'erreurs :** `tail -n 50 /var/log/apache2/error.log`, ou `/var/log/httpd/error_log`.
4. **Disque et mémoire :** tous deux peuvent arrêter Apache ([espace disque](surveiller-espace-disque.md), [CPU et mémoire](surveiller-cpu-memoire.md)).

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
