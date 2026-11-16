---
title: "Comment surveiller les erreurs du journal d'événements Windows"
description: "Soyez alerté quand Windows écrit un événement critique ou d'erreur dans le journal Système ou Application, sur un serveur ou tout un site."
slug: surveiller-journal-evenements-windows
lang: fr
translation: monitor-windows-event-log
---

# Comment surveiller les erreurs du journal d'événements Windows

Windows consigne ce qui ne va pas dans ses journaux d'événements : un service planté, un disque qui signale des secteurs défectueux, une mise à jour en échec. Personne ne lit l'Observateur d'événements chaque matin : ces erreurs y attendent qu'on cherche la cause d'une panne. MonitorFlow vous prévient quand une nouvelle erreur est écrite.

## Pourquoi c'est important

Beaucoup de pannes s'annoncent. Un contrôleur de disque consigne des erreurs pendant des jours avant qu'un volume ne disparaisse. Un service qui plante puis redémarre seul ne laisse qu'un événement. Être prévenu dès l'écriture de l'erreur vous permet d'agir avant la panne.

## Ce que MonitorFlow vérifie

La vérification « Windows logs an error » compte les nouveaux événements **critiques** et **d'erreur** d'un journal. Le catalogue propose deux entrées :

| Entrée du catalogue | Journal |
| --- | --- |
| Errors in the System event log | `System` : services, pilotes, disques, noyau |
| Errors in the Application event log | `Application` : applications et leurs services |

Fonctionnement :

- Seuls les événements écrits après l'ajout de la vérification comptent : l'historique ne déclenche pas d'alerte pour les erreurs du mois dernier.
- À chaque passage, l'agent compte les événements écrits depuis le précédent. La vérification porte le nom du journal, par exemple `eventlog_system`, et son alerte surveille `eventlog_system_matches`, le nombre de nouvelles erreurs.
- Par défaut, l'alerte est un **avertissement** dès la première nouvelle erreur, et se résout quand un passage n'en trouve aucune.
- L'agent lit `Application` et `System` par défaut. Un autre journal, comme `Microsoft-Windows-TaskScheduler/Operational`, doit d'abord figurer dans `event_channels`, sous `[sensors]` dans le fichier `agent.toml` du serveur, agent redémarré : ce qu'il peut lire se décide sur le serveur lui-même.

Les événements sont comptés, pas copiés, et les journaux Windows ne s'affichent pas encore dans l'onglet « Logs » : l'alerte dit combien d'erreurs sont arrivées, l'Observateur d'événements dit lesquelles.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur, restez sur l'onglet « Sensors » et cliquez sur « Add a check ».
2. Dans « Find a check », tapez `event` et choisissez « Errors in the System event log » : le champ « Event log » contient `System`.
3. Laissez cochée « Raise warning when it fails ». Sous « Add to », choisissez « This server only » ou « Every server of » son site, puis « Add check ».
4. Ajoutez de même « Errors in the Application event log » si vous voulez les deux.

Pour tous les serveurs Windows d'un site : « Settings › Sites », puis « Alerts and checks » pour ce site, et « Add a check ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit » :

- « Alert » : avertissement ou critique sur une erreur ;
- « For at least » : 0 alerte dès la première erreur ; quelques minutes, et l'alerte ne s'ouvre que si les erreurs continuent d'un passage à l'autre, ce qui calme un journal bavard ;
- « Apply to » : ce serveur seulement, ou tous les serveurs du site.

Pour un site, ces réglages sont sous « When to alert ». Commencez par le journal Système, d'ordinaire plus calme, et ajoutez le journal Application là où ses erreurs ont un sens.

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une même vérification en alerte sur plusieurs serveurs part dans un seul message. Un avertissement est signalé une fois ; une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Warning: eventlog_system_matches is 3 on app-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Cliquez sur « Acknowledge », puis :

1. Ouvrez l'**Observateur d'événements**, ou lancez `Get-WinEvent -FilterHashtable @{LogName='System'; Level=1,2} -MaxEvents 20` pour les derniers événements critiques et d'erreur.
2. Regardez la **source** et l'**ID de l'événement** : ils désignent le service, le pilote ou l'application en cause.
3. Pour un service arrêté, ajoutez une vérification qu'il tourne : voir [Comment surveiller les services Windows](surveiller-services-windows.md).

Pour des fichiers journaux texte, voir [Comment surveiller les fichiers journaux](surveiller-fichiers-journaux.md).

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web, choisissez Windows, et lancez la ligne affichée dans PowerShell en tant qu'administrateur sur chaque serveur Windows. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
