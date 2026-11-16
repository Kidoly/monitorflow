---
title: "Comment surveiller SQL Server"
description: "Soyez alerté quand une instance SQL Server s'arrête ou refuse les connexions sous Windows, avec les vérifications du service MSSQLSERVER et du port 1433."
slug: surveiller-sql-server
lang: fr
translation: monitor-sql-server
---

# Comment surveiller SQL Server

Un ERP, un logiciel de comptabilité ou une application métier dépend souvent d'une seule instance SQL Server sur un seul serveur Windows. Quand elle s'arrête, tous ses utilisateurs sont bloqués. MonitorFlow surveille le service SQL Server et son port, et vous prévient en une à deux minutes.

## Pourquoi c'est important

SQL Server s'arrête après une mise à jour Windows qui a redémarré le serveur en laissant le service arrêté, quand un lecteur de données ou de journaux se remplit, quand le mot de passe du compte de service a changé, ou après un plantage. Les utilisateurs appellent : l'application « ne s'ouvre pas », et l'on cherche longtemps quel serveur est en cause.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « SQL Server running » | Le service Windows `MSSQLSERVER`, l'instance par défaut, est démarré. Métrique `service_mssqlserver_active` : 1 démarré, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « SQL Server answers » | Le port 1433 accepte une connexion TCP sur 127.0.0.1. Métrique `port_1433_up` | Critique en cas d'échec, pendant 1 minute |

**Les instances nommées** tournent sous `MSSQL$` suivi du nom de l'instance, par exemple `MSSQL$SQLEXPRESS` : saisissez le nom du service, pas son nom d'affichage. Une instance nommée choisit en général son port au démarrage : fixez-le dans le Gestionnaire de configuration SQL Server avant de la vérifier. SQL Server Express est livré avec TCP/IP désactivé : la vérification de port échoue tant que vous ne l'activez pas.

Un service absent compte comme arrêté : un mauvais nom déclenche une alerte. Pour l'Agent SQL Server, ajoutez « A Windows service is running » avec `SQLSERVERAGENT` (ou `SQLAgent$` suivi du nom de l'instance).

SQL Server écrit ses erreurs dans le journal d'événements Application : « Errors in the Application event log » lève un avertissement à chaque nouvel événement d'erreur ou critique. L'espace disque de chaque lecteur est vérifié sans configuration.

Sous Linux, utilisez les vérifications génériques : « A service is running » avec `mssql-server`, et « A port answers » avec 1433.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur Windows et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `SQL Server` dans « Find a check ».
3. Choisissez « SQL Server running ». Gardez `MSSQLSERVER` dans « Service » ou saisissez votre instance nommée.
4. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
5. Faites de même avec « SQL Server answers », avec votre port s'il diffère de 1433.

Les vérifications tournent toutes les 60 secondes par défaut. Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site et utilisez « Add a check », mais seulement si tous ses serveurs font tourner SQL Server : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la vérification et choisissez « Edit ». « Alert » règle critique ou avertissement, « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite ; plus long évite une alerte quand Windows Update redémarre le serveur), « Apply to » ce serveur ou tous ceux du site. Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: service_mssqlserver_active MSSQLSERVER is 0 on sql-1 (paris)
Critical: port_1433_up is 0 on sql-1 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand SQL Server revient.

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **Le service :** `Get-Service MSSQLSERVER` dans PowerShell, puis `Start-Service MSSQLSERVER` s'il suffit de le démarrer.
2. **La cause de l'arrêt :** les journaux Application et Système dans l'Observateur d'événements, et le `ERRORLOG` de SQL Server dans le dossier `Log` de l'instance.
3. **L'espace disque :** un lecteur de données ou de journaux plein arrête les bases. L'onglet « Sensors » affiche chaque lecteur.
4. **Le service tourne mais le port échoue :** vérifiez dans le Gestionnaire de configuration SQL Server que TCP/IP est activé et sur quel port, ainsi que le pare-feu Windows.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web, choisissez Windows, et lancez la ligne affichée dans PowerShell en tant qu'administrateur sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md#windows) (en anglais).
