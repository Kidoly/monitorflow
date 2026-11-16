---
title: "Comment surveiller les services Windows"
description: "Soyez alerté quand un service Windows s'arrête ou disparaît, comme Windows Update, le spouleur d'impression, le serveur DNS ou le service de temps."
slug: surveiller-services-windows
lang: fr
translation: monitor-windows-services
---

# Comment surveiller les services Windows

L'essentiel de ce que fait un serveur Windows tourne sous forme de services : serveur web, base de données, file d'impression, horloge, mises à jour. Quand l'un d'eux s'arrête, Windows ne prévient personne. MonitorFlow vérifie les services que vous choisissez et vous alerte quand l'un d'eux ne tourne plus.

## Pourquoi c'est important

Un service s'arrête après un plantage, une mise à jour ratée ou un mot de passe expiré sur son compte, et beaucoup ne redémarrent pas seuls. Le serveur répond toujours au ping : le premier signe vient souvent d'un utilisateur. L'impression ne marche plus, ou les mises à jour sont bloquées depuis des semaines.

## Ce que MonitorFlow vérifie

La vérification « A Windows service is running » interroge le gestionnaire de services sur un service, désigné par son **nom de service** et non son nom complet : `W32Time`, et non « Temps Windows ». Ce nom figure sous « Nom du service » dans ses propriétés, ou dans la colonne `Name` de `Get-Service`.

Le catalogue propose les services courants, nom déjà rempli :

| Entrée du catalogue | Nom du service |
| --- | --- |
| Windows Update running | `wuauserv` |
| Print Spooler running | `Spooler` |
| DNS Server running | `DNS` |
| Windows Time running | `W32Time` |
| IIS running | `W3SVC` |
| SQL Server running | `MSSQLSERVER` |
| Active Directory running | `NTDS` |

Un service en pause compte comme arrêté. Un service absent, supprimé ou mal saisi, aussi : une erreur de nom déclenche une alerte au lieu de passer inaperçue.

La vérification porte le nom du service : pour `Spooler`, `service_spooler`, dont l'alerte surveille `service_spooler_active`, avec le nom du service comme instance. Par défaut, l'alerte est **critique** quand le service est arrêté depuis **1 minute**.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur, restez sur l'onglet « Sensors » et cliquez sur « Add a check ».
2. Dans « Find a check », tapez le service, par exemple `spooler`, et choisissez l'entrée. Pour un autre service, prenez « A Windows service is running » et saisissez son nom dans « Service ».
3. Laissez cochée « Raise critical when it fails ».
4. Sous « Add to », choisissez « This server only » ou « Every server of » son site, puis « Add check ».

Pour tous les serveurs d'un site : « Settings › Sites », puis « Alerts and checks » pour ce site, et « Add a check » sous « What is checked ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit » :

- « Alert » : critique ou avertissement quand le service s'arrête ;
- « For at least » : les minutes d'arrêt avant l'alerte ; 0 alerte dès la première mauvaise valeur ;
- « Apply to » : ce serveur seulement, ou tous les serveurs du site.

Pour un site, ces réglages sont sous « When to alert », sur sa page « Alerts and checks ».

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Un même service arrêté sur plusieurs serveurs part dans un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: service_spooler_active Spooler is 0 on print-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Quand le service repart, suit `Resolved: service_spooler_active Spooler on print-1 (paris)`.

Cliquez sur « Acknowledge », puis vérifiez :

1. **Son état** : `Get-Service Spooler`. Arrêté, ou absent ?
2. **La cause** : le journal Système contient les événements du gestionnaire de services, comme 7031 ou 7034 pour un arrêt inattendu. Pour en être prévenu, voir [Comment surveiller le journal d'événements Windows](surveiller-journal-evenements-windows.md).
3. **Redémarrez-le** : `Start-Service Spooler`. S'il s'arrête encore, vérifiez son compte.

Pour ce qu'un service ne montre pas, comme la longueur d'une file, voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web, choisissez Windows, et lancez la ligne affichée dans PowerShell en tant qu'administrateur sur chaque serveur Windows. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
