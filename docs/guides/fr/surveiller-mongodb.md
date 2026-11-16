---
title: "Comment surveiller MongoDB"
description: "Soyez alerté quand MongoDB s'arrête ou n'accepte plus de connexions sur un serveur Linux, avec les vérifications du service mongod et du port 27017."
slug: surveiller-mongodb
lang: fr
translation: monitor-mongodb
---

# Comment surveiller MongoDB

Quand `mongod` s'arrête, les applications qui en dépendent échouent sur des délais dépassés et des erreurs de connexion, souvent loin de la base. MonitorFlow surveille le service MongoDB et son port, et vous prévient en une à deux minutes.

## Pourquoi c'est important

MongoDB s'arrête pour les mêmes raisons que toute base : volume de données plein, noyau qui le tue faute de mémoire, mise à jour qui a changé le format de configuration, redémarrage sans service activé. Sa consommation de mémoire grandit avec les données en cache : un serveur stable depuis des mois peut basculer quand les données grossissent.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « MongoDB running » | Le service systemd `mongod` est actif. Métrique `service_mongod_active` : 1 actif, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « MongoDB answers » | Le port 27017 accepte une connexion TCP sur 127.0.0.1. Métrique `port_27017_up` | Critique en cas d'échec, pendant 1 minute |

`mongod` est le nom d'unité des paquets officiels. Un service absent compte comme arrêté : un mauvais nom déclenche une alerte. Avec plusieurs instances sur un serveur, ajoutez une vérification de service et une de port par instance, avec leurs unités et ports.

Une vérification de port prouve que MongoDB accepte les connexions TCP, pas qu'il répond aux requêtes ni qu'un replica set a un primaire. Pour cela, un script de votre cru peut interroger le serveur et signaler OK, avertissement ou critique par son code de sortie. Voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

La mémoire et l'espace disque sont vérifiés sur chaque serveur sans configuration : disque en avertissement à 85 % de remplissage et critique à 95 %, mémoire en avertissement sous 10 % disponible et critique sous 5 %.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `MongoDB` dans « Find a check ».
3. Choisissez « MongoDB running ». Gardez `mongod` dans « Service », sauf si votre unité porte un autre nom.
4. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
5. Faites de même avec « MongoDB answers ». Si MongoDB n'écoute que sur une autre adresse (`net.bindIp` dans `/etc/mongod.conf`), saisissez-la dans « Address ».

Les vérifications tournent toutes les 60 secondes par défaut. Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site et utilisez « Add a check », mais seulement si tous ses serveurs font tourner MongoDB : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la vérification et choisissez « Edit ». « Alert » règle critique ou avertissement (un membre de replica set remplaçable peut mériter un avertissement), « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite), « Apply to » ce serveur ou tous ceux du site. Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: service_mongod_active mongod.service is 0 on mongo-1 (paris)
Critical: port_27017_up is 0 on mongo-1 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand MongoDB revient.

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La cause de l'arrêt :** `systemctl status mongod` et `journalctl -u mongod -n 50`, puis le journal de MongoDB, `/var/log/mongodb/mongod.log` avec les paquets officiels.
2. **L'espace disque :** `df -h` sur le volume de `storage.dbPath`.
3. **La mémoire :** `journalctl -k | grep -i oom` montre si le noyau l'a tué.
4. **Le service tourne mais le port échoue :** vérifiez `net.bindIp` et `net.port` dans `/etc/mongod.conf`, et le pare-feu local.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
