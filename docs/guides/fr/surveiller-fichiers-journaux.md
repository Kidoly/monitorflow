---
title: "Comment surveiller les erreurs des fichiers journaux sous Linux et Windows"
description: "Soyez alerté quand une erreur, un échec de connexion ou un motif choisi apparaît dans un fichier journal, et lisez les journaux système au même endroit."
slug: surveiller-fichiers-journaux
lang: fr
translation: monitor-log-files
---

# Comment surveiller les erreurs des fichiers journaux sous Linux et Windows

La plupart des pannes sont écrites avant d'être remarquées, dans des fichiers que personne ne lit. MonitorFlow compte les lignes qui comptent et vous alerte dès qu'il en apparaît une.

## Pourquoi c'est important

Une sauvegarde qui échoue chaque nuit, un module qui expire une fois par heure, des erreurs d'E/S sur un disque près de lâcher : chacun laisse une ligne dans un fichier, et rien d'autre. Quand un utilisateur se plaint, le journal contient des semaines d'avertissements.

## Ce que MonitorFlow vérifie

La vérification « A log file gets a line » compte, à chaque passage, les nouvelles lignes d'un fichier qui correspondent à un motif : un texte ou une expression régulière. Le catalogue en propose de prêtes à l'emploi pour Linux :

| Entrée du catalogue | Fichier | Lignes contenant |
| --- | --- | --- |
| « Errors in syslog » | `/var/log/syslog` | `error` |
| « Failed SSH logins » | `/var/log/auth.log` | `Failed password` |
| « Out of memory kills » | `/var/log/kern.log` | `Out of memory\|oom-kill` |
| « nginx critical errors » | `/var/log/nginx/error.log` | `\[(crit\|alert\|emerg)\]` |

Sa métrique est le nombre de lignes trouvées depuis le passage précédent, nommée d'après le fichier : `log_syslog_matches` pour `/var/log/syslog`.

Comment elle lit les fichiers :

- Seules les lignes écrites après l'ajout de la vérification sont comptées, jamais l'historique du fichier. Après un redémarrage ou une mise à jour de l'agent, elle reprend là où elle s'était arrêtée.
- Elle suit le fichier lors de la rotation des journaux, renommé ou tronqué.
- Sous Linux, elle ne lit que les fichiers sous `/var/log`. Pour surveiller des fichiers ailleurs, l'administrateur du serveur indique les dossiers dans `log_dirs`, sous `[sensors]` dans `/etc/monitorflow/agent.toml`, puis redémarre l'agent. Sous Windows, la liste est vide tant que personne ne la remplit, par exemple `log_dirs = ['D:\Logs\MyApp']`, en donnant au service de l'agent le droit de lire le dossier.
- L'agent tourne sous l'utilisateur `monitorflow`, qui ne peut pas lire au départ la plupart des fichiers de `/var/log`. Sous Debian et Ubuntu, `syslog`, `auth.log` et `kern.log` appartiennent au groupe `adm` : ajoutez-y l'utilisateur avec `sudo usermod -aG adm monitorflow`, puis `sudo systemctl restart monitorflow-agent`. L'agent peut alors lire tous les journaux de ce groupe. D'ici là, la ligne de la vérification reste sur « Waiting for first value ».
- Seuls les comptes remontent au serveur central, pas les lignes.

Par ailleurs, chaque serveur Linux envoie les avertissements et erreurs de son journal systemd, sans rien configurer : lisez-les dans l'onglet « Logs » du serveur. Pour Windows, voir [Comment surveiller le journal d'événements Windows](surveiller-journal-evenements-windows.md).

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur son onglet « Sensors ».
2. Choisissez « Add a check » et tapez `log` dans « Find a check ». Prenez une entrée toute prête, ou « A log file gets a line » pour votre propre fichier.
3. Remplissez « Log file », le chemin complet, et « Lines containing », le texte ou l'expression régulière à chercher, comme `ERROR|FATAL` ou `timeout after [0-9]+ ms`.
4. Laissez « Raise warning when it fails » coché.
5. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis « Add check ».

Pour tout un site, passez par « Settings › Sites », puis « Alerts and checks », et « Add a check ».

## Modifier l'alerte

Par défaut, une seule ligne correspondante lève aussitôt un **avertissement**. Dans l'onglet « Sensors », ouvrez le menu de la ligne de la vérification et choisissez « Edit » :

- « Alert » : passez-la en critique, pour les lignes qui signifient une panne.
- « For at least » : un nombre de minutes. Chaque passage compte les lignes de son intervalle ; avec 5 minutes, l'alerte ne s'ouvre que si des lignes arrivent pendant 5 minutes d'affilée.
- « Apply to » : ce serveur seulement, ou tous les serveurs du site.

L'alerte se résout d'elle-même au premier passage qui ne trouve aucune nouvelle ligne.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique combien de lignes ont correspondu lors du dernier passage, et sur quel serveur :

```
Warning: log_syslog_matches is 3 on web-1 (paris)
```

Cliquez sur « Acknowledge » sur la page de l'alerte, puis lisez les lignes sur le serveur autour de cette heure : `grep -i error /var/log/syslog | tail -50`, ou `Select-String -Path D:\Logs\MyApp\app.log -Pattern ERROR | Select-Object -Last 50` sous Windows. Sous Linux, l'onglet « Logs » montre souvent le même problème vu du journal systemd.

Si elle se déclenche sur des lignes sans importance, resserrez le motif : `error` correspond aussi à `0 errors`.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
