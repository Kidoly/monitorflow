---
title: "Comment surveiller le Bureau à distance (RDP)"
description: "Soyez alerté quand le Bureau à distance ne répond plus sur le port 3389 ou que son service s'arrête, sur vos serveurs Windows."
slug: surveiller-bureau-a-distance
lang: fr
translation: monitor-remote-desktop
---

# Comment surveiller le Bureau à distance (RDP)

Le Bureau à distance est l'accès des administrateurs à leurs serveurs Windows et, sur un serveur de terminaux, le poste de travail quotidien des utilisateurs. Quand il ne répond plus, vous perdez votre accès, souvent au pire moment. MonitorFlow vérifie que le Bureau à distance répond et que son service tourne.

## Pourquoi c'est important

Le Bureau à distance cesse de répondre après un plantage du service, une mise à jour qui a modifié un réglage, ou une règle de pare-feu poussée par stratégie. Sur un serveur d'administration, vous le découvrez en voulant vous connecter pendant un incident ; sur un serveur de terminaux, vos utilisateurs le découvrent les premiers, tous en même temps.

## Ce que MonitorFlow vérifie

| Vérification | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| Remote Desktop answers (catalogue) | Le port 3389 accepte les connexions | Critique après 1 minute |
| A Windows service is running, avec `TermService` | Le service Services Bureau à distance tourne | Critique après 1 minute |

« Remote Desktop answers » se connecte au port 3389, par défaut sur le serveur lui-même (`127.0.0.1`). Elle s'appelle `port_3389` et son alerte surveille `port_3389_up`. Elle n'ouvre pas de session : le port répond, sans garantir que les connexions aboutissent.

Le service Services Bureau à distance s'appelle `TermService`. Le catalogue n'a pas d'entrée pour lui : utilisez la vérification générique « A Windows service is running » avec ce nom. Elle s'appelle `service_termservice` et son alerte surveille `service_termservice_active`, avec `TermService` comme instance. Un service absent ou en pause compte comme arrêté.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur Windows, restez sur l'onglet « Sensors » et cliquez sur « Add a check ».
2. Dans « Find a check », tapez `remote desktop` et choisissez « Remote Desktop answers ». Gardez `3389` dans « Port » (ou votre port RDP) et `127.0.0.1` dans « Address ».
3. Laissez cochée « Raise critical when it fails ». Sous « Add to », choisissez « This server only » ou « Every server of » son site, puis « Add check ».
4. Recommencez avec « A Windows service is running », `TermService` dans « Service ».

Une vérification sur `127.0.0.1` ne voit pas un pare-feu qui bloque le port 3389 depuis le réseau. Pour tester comme vous vous connectez, ajoutez la vérification de port sur **un autre serveur** du réseau, avec le nom du serveur Windows dans « Address ». Sur un serveur Windows, l'entrée « Remote Desktop answers » convient telle quelle ; sur un serveur Linux, prenez « A port answers » et saisissez `3389`.

Pour tous les serveurs Windows d'un site : « Settings › Sites », puis « Alerts and checks » pour ce site, et « Add a check ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». Réglez « Alert » (critique ou avertissement), « For at least » (en minutes ; 0 alerte dès la première mauvaise valeur) et « Apply to » (ce serveur ou tout le site). Pour un site, ces réglages sont sous « When to alert ». Pour un redémarrage prévu, utilisez « Silence for » sur la page du serveur.

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une même vérification en échec sur plusieurs serveurs part dans un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: port_3389_up is 0 on ts-1 (paris)
Critical: service_termservice_active TermService is 0 on ts-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Cliquez sur « Acknowledge », puis vérifiez :

1. **Le service** : `Get-Service TermService`, et `Start-Service TermService` s'il s'est arrêté.
2. **L'autorisation** : le Bureau à distance est-il toujours activé, dans les propriétés système ou par stratégie de groupe ?
3. **Le pare-feu** : `Test-NetConnection <serveur> -Port 3389` depuis une autre machine, et les règles du Bureau à distance sur le serveur.
4. **Le journal Système**, pour la raison de l'arrêt du service.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web, choisissez Windows, et lancez la ligne affichée dans PowerShell en tant qu'administrateur sur chaque serveur Windows. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
