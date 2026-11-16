---
title: "Comment surveiller les contrôleurs de domaine Active Directory"
description: "Surveillez vos contrôleurs de domaine : Active Directory, serveur DNS, LDAP sur le port 389, service de temps et erreurs du journal Système."
slug: surveiller-active-directory
lang: fr
translation: monitor-active-directory
---

# Comment surveiller les contrôleurs de domaine Active Directory

Quand un contrôleur de domaine tombe, les ouvertures de session échouent, les stratégies de groupe ne s'appliquent plus et la résolution de noms casse. Avec deux contrôleurs, le second masque souvent la panne jusqu'à ce qu'il tombe à son tour. MonitorFlow vérifie les services dont dépend un contrôleur et vous alerte quand l'un d'eux s'arrête.

## Pourquoi c'est important

Un contrôleur de domaine repose sur les services de domaine Active Directory, le DNS pour que les clients le trouvent, LDAP pour les applications, et une horloge juste, car Kerberos refuse les tickets dès que les horloges divergent de quelques minutes. Chacun peut s'arrêter après une mise à jour ou un redémarrage, et les utilisateurs n'en voient que les effets : « mon mot de passe ne marche pas ».

## Ce que MonitorFlow vérifie

Une entrée du catalogue par élément, à ajouter sur chaque contrôleur :

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| Active Directory running | Le service `NTDS` tourne | Critique après 1 minute |
| DNS Server running | Le service `DNS` tourne | Critique après 1 minute |
| LDAP answers | Le port 389 accepte les connexions | Critique après 1 minute |
| Windows Time running | Le service `W32Time` tourne | Critique après 1 minute |
| Errors in the System event log | Événements critiques et d'erreur du journal Système | Avertissement à chaque nouvelle erreur |

Les vérifications de service s'appellent `service_ntds`, `service_dns` et `service_w32time` ; leurs alertes surveillent `service_ntds_active` et ainsi de suite, avec le nom du service comme instance. Un service absent ou en pause compte comme arrêté. `port_389` se connecte au port 389 du serveur (alerte sur `port_389_up`). `eventlog_system` compte les nouveaux événements critiques et d'erreur (alerte sur `eventlog_system_matches`).

La vérification du service de temps indique qu'il tourne, pas l'écart de l'horloge.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le contrôleur, restez sur l'onglet « Sensors » et cliquez sur « Add a check ».
2. Dans « Find a check », tapez `active directory` et choisissez « Active Directory running » : le champ « Service » contient `NTDS`.
3. Laissez cochée « Raise critical when it fails », choisissez « This server only » sous « Add to », puis « Add check ».
4. Faites de même pour « DNS Server running », « LDAP answers », « Windows Time running » et « Errors in the System event log ».

Si vos contrôleurs ont leur propre site dans MonitorFlow, ajoutez les vérifications une fois pour tous : « Settings › Sites », puis « Alerts and checks » pour ce site, et « Add a check ». Un contrôleur ajouté plus tard au site les reçoit aussi.

Pour savoir si les clients joignent LDAP, ajoutez aussi « LDAP answers » sur un serveur membre, avec le nom du contrôleur dans « Address ».

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». Réglez « Alert » (critique ou avertissement), « For at least » (en minutes ; 0 alerte dès la première mauvaise valeur) et « Apply to » (ce serveur ou tout le site). Pour un site, ces réglages sont sous « When to alert ».

Si la vérification du journal est trop bavarde, augmentez son « For at least » : l'alerte ne s'ouvre que si les erreurs continuent.

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une même vérification en échec sur plusieurs contrôleurs part dans un seul message. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: service_ntds_active NTDS is 0 on dc-1 (paris)
Critical: port_389_up is 0 on dc-1 (paris)
Warning: eventlog_system_matches is 2 on dc-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Cliquez sur « Acknowledge », puis vérifiez :

1. **Les services** : `Get-Service NTDS, DNS, W32Time`, et `Start-Service` pour celui qui s'est arrêté.
2. **La santé du contrôleur** : `dcdiag` et `repadmin /replsummary` montrent les tests en échec et les erreurs de réplication.
3. **L'horloge** : `w32tm /query /status` indique la source de temps et la dernière synchronisation.
4. **Les journaux** : Système, Service d'annuaire et Serveur DNS dans l'Observateur d'événements.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web, choisissez Windows, et lancez la ligne affichée dans PowerShell en tant qu'administrateur sur chaque serveur Windows. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
