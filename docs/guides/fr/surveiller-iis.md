---
title: "Comment surveiller IIS"
description: "Soyez alerté quand le serveur web IIS s'arrête ou ne répond plus sur les ports 80 et 443 de vos serveurs Windows."
slug: surveiller-iis
lang: fr
translation: monitor-iis
---

# Comment surveiller IIS

IIS sert des sites, des intranets, des API et la partie web de nombreuses applications métier sous Windows Server. Quand il s'arrête, tous les sites du serveur tombent avec lui. MonitorFlow vérifie que le service IIS tourne et que ses ports répondent.

## Pourquoi c'est important

IIS s'arrête après une mise à jour ratée, une mauvaise configuration ou un redémarrage qui ne l'a pas relancé. Un port peut aussi cesser de répondre alors que le service tourne : liaison supprimée, règle de pare-feu. Les utilisateurs voient une erreur ; vous l'apprenez quand ils appellent.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| IIS running | Le service `W3SVC` tourne | Critique après 1 minute |
| Web server answers (HTTP) | Le port 80 accepte les connexions | Critique après 1 minute |
| Web server answers (HTTPS) | Le port 443 accepte les connexions | Critique après 1 minute |

« IIS running » crée la vérification `service_w3svc`, dont l'alerte surveille `service_w3svc_active`, avec `W3SVC` comme instance. Un service en pause ou absent compte comme arrêté.

Les vérifications de port, `port_80` et `port_443`, se connectent par défaut au serveur lui-même (`127.0.0.1`) ; leurs alertes surveillent `port_80_up` et `port_443_up`. Elles ne lisent ni la page ni le certificat : un site qui répond par une erreur passe. Pour cela, voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

La vérification « A log file gets a line » ne peut pas encore suivre les journaux d'IIS, nommés d'après la date. Ses erreurs dans les journaux Système et Application se surveillent, voir [Comment surveiller le journal d'événements Windows](surveiller-journal-evenements-windows.md).

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur, restez sur l'onglet « Sensors » et cliquez sur « Add a check ».
2. Dans « Find a check », tapez `iis` et choisissez « IIS running » : le champ « Service » contient `W3SVC`.
3. Laissez cochée « Raise critical when it fails ». Sous « Add to », choisissez « This server only » ou « Every server of » son site, puis « Add check ».
4. Recommencez avec « Web server answers (HTTPS) », `443` dans « Port ». Si le site n'écoute que sur une adresse, saisissez-la dans « Address ». Ajoutez la vérification HTTP si vous servez le port 80.

Pour vérifier depuis l'extérieur, ajoutez la vérification de port sur un autre serveur, avec le nom du serveur web dans « Address » : elle détecte aussi un pare-feu qui bloque le port.

Pour tous les serveurs d'un site : « Settings › Sites », puis « Alerts and checks » pour ce site, et « Add a check ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». Réglez « Alert » (critique ou avertissement), « For at least » (en minutes ; 0 alerte dès la première mauvaise valeur) et « Apply to » (ce serveur ou tout le site). Pour un site, ces réglages sont sous « When to alert ». Pour un redémarrage prévu, utilisez « Silence for » sur la page du serveur.

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Les alertes qui s'ouvrent en même temps sur un site, comme le service et ses ports, partent dans un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: service_w3svc_active W3SVC is 0 on web-1 (paris)
Critical: port_443_up is 0 on web-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Cliquez sur « Acknowledge », puis vérifiez :

1. **Le service** : `Get-Service W3SVC`, puis `Start-Service W3SVC` s'il est arrêté, et la cause dans le journal Système.
2. **Le site et ses liaisons** dans le Gestionnaire IIS : site démarré, port et certificat toujours liés ?
3. **Le pool d'applications** : un pool arrêté renvoie des erreurs alors que le port répond.
4. **Le pare-feu**, si seule la vérification depuis un autre serveur échoue.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web, choisissez Windows, et lancez la ligne affichée dans PowerShell en tant qu'administrateur sur chaque serveur Windows. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
