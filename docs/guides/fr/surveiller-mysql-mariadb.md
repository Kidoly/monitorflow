---
title: "Comment surveiller MySQL et MariaDB"
description: "Soyez alerté quand MySQL ou MariaDB s'arrête ou n'accepte plus de connexions, avec les vérifications de service et de port du catalogue MonitorFlow."
slug: surveiller-mysql-mariadb
lang: fr
translation: monitor-mysql-mariadb
---

# Comment surveiller MySQL et MariaDB

Un site web, un CRM ou un outil de tickets peut tourner des années sur un serveur MySQL ou MariaDB que personne ne regarde, jusqu'au jour où il s'arrête. MonitorFlow surveille le service et son port, et vous prévient en une à deux minutes, avant vos utilisateurs.

## Pourquoi c'est important

MySQL et MariaDB s'arrêtent pour des raisons ordinaires : disque rempli de journaux binaires, noyau qui tue le serveur faute de mémoire, mise à jour qui laisse une configuration refusée, récupération longue après un redémarrage. L'application affiche « Error establishing a database connection » ou une page blanche, et le premier signalement vient d'un client.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « MySQL or MariaDB running » (Linux) | Le service systemd `mariadb` est actif. Métrique `service_mariadb_active` : 1 actif, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « MySQL or MariaDB answers » (Linux et Windows) | Le port 3306 accepte une connexion TCP sur 127.0.0.1. Métrique `port_3306_up` | Critique en cas d'échec, pendant 1 minute |

**Choisissez le bon nom d'unité :** `mariadb` pour MariaDB, `mysql` pour MySQL sous Debian et Ubuntu, `mysqld` pour MySQL sous RHEL et Rocky. La métrique suit : avec `mysql`, c'est `service_mysql_active`. Un service absent compte comme arrêté : un mauvais nom déclenche une alerte au lieu de masquer le problème.

Une vérification de port prouve que le serveur accepte les connexions TCP, pas qu'il répond aux requêtes. Pour aller plus loin, appelez `mysqladmin ping` depuis un script : il sort avec 0 quand le serveur tourne, même sans identifiants valides, et 1 sinon. Voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

L'espace disque est vérifié sur chaque serveur sans configuration : avertissement à 85 % de remplissage, critique à 95 %, avant que les journaux binaires ne remplissent le disque.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `MariaDB` ou `MySQL` dans « Find a check ».
3. Choisissez « MySQL or MariaDB running ». Dans « Service », gardez `mariadb` ou saisissez `mysql` ou `mysqld`.
4. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
5. Faites de même avec « MySQL or MariaDB answers ». Si le serveur n'écoute que sur une autre adresse (`bind-address`), saisissez-la dans « Address ».

Les vérifications tournent toutes les 60 secondes par défaut. Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site et utilisez « Add a check », mais seulement si tous ses serveurs font tourner la base : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la vérification et choisissez « Edit ». « Alert » règle critique ou avertissement, « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite ; plus long pour une base lente à redémarrer après une mise à jour), « Apply to » ce serveur ou tous ceux du site. Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: service_mariadb_active mariadb.service is 0 on db-2 (lyon)
Critical: port_3306_up is 0 on db-2 (lyon)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand la base revient.

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La cause de l'arrêt :** `systemctl status mariadb` et `journalctl -u mariadb -n 50` (ou votre unité). L'onglet « Logs » du serveur affiche aussi les avertissements et erreurs du journal.
2. **L'espace disque :** `df -h`. Journaux binaires et grosses tables temporaires remplissent vite un volume.
3. **La mémoire :** `journalctl -k | grep -i oom` montre si le noyau a tué `mysqld` ou `mariadbd`.
4. **Le service tourne mais le port échoue :** vérifiez `bind-address`, `port` et `skip-networking`, et le pare-feu local.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
