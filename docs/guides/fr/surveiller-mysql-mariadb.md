---
title: "Comment surveiller MySQL et MariaDB"
description: "Suivez les connexions, requêtes lentes, le buffer pool InnoDB et la réplication de MySQL et MariaDB, et soyez alerté quand la base ne répond plus, avec le contrôle prêt à l'emploi de MonitorFlow."
slug: surveiller-mysql-mariadb
lang: fr
translation: monitor-mysql-mariadb
---

# Comment surveiller MySQL et MariaDB

Un site web, un CRM ou un outil de tickets peut tourner des années sur un serveur MySQL ou MariaDB que personne ne regarde, jusqu'au jour où il s'arrête. MonitorFlow se connecte avec un compte en lecture seule, lit l'état du serveur, et vous prévient en une à deux minutes quand il ne répond plus ou commence à peiner, avant vos utilisateurs.

## Pourquoi c'est important

MySQL et MariaDB s'arrêtent pour des raisons ordinaires : disque rempli de journaux binaires, noyau qui tue le serveur faute de mémoire, mise à jour qui laisse une configuration refusée, récupération longue après un redémarrage. L'application affiche « Error establishing a database connection » ou une page blanche, et le premier signalement vient d'un client.

Les ennuis commencent souvent plus tôt : les connexions atteignent `max_connections`, les requêtes lentes s'accumulent, le buffer pool est trop petit pour les données, un réplica cesse de copier. L'état du serveur le montre en premier.

## Ce que MonitorFlow vérifie

### Le contrôle MySQL ou MariaDB

L'entrée « MySQL ou MariaDB » du catalogue (un « Contrôle d’application ») fonctionne avec MySQL 5.7 ou plus récent et MariaDB 10.3 ou plus récent. Chaque minute, il se connecte et lit :

- s'il répond et si la connexion du compte réussit, et en combien de temps ;
- les connexions par rapport à `max_connections`, et les requêtes en cours ;
- les requêtes par seconde, les requêtes lentes (au-delà de `long_query_time`) et les tentatives de connexion échouées ;
- le taux de succès du buffer pool InnoDB et les verrous de lignes attendus ;
- la taille de chaque schéma que le compte voit, toutes les 10 minutes ;
- sur un réplica, si ses threads I/O et SQL tournent, et son retard.

Les métriques portent le nom du contrôle : `monitorflow_mysql_up`, `monitorflow_mysql_connections_ratio`, etc. (avec un autre nom de contrôle, ce nom remplace `mysql`).

Avec « Ajouter les alertes recommandées » activé, ces alertes l'accompagnent :

| Métrique | Avertissement | Critique | Pendant |
| --- | --- | --- | --- |
| `monitorflow_mysql_up` (il répond et la connexion réussit) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_mysql_connections_ratio` | Au-delà de 0,8 (80 %) | Au-delà de 0,95 | 5 minutes |
| `monitorflow_mysql_slow_queries` (depuis le contrôle précédent) | Au-delà de 10 | Aucun | 5 minutes |
| `monitorflow_mysql_buffer_pool_hit_ratio` | Sous 0,95 | Aucun | 30 minutes |
| `monitorflow_mysql_lock_waits` | Au-delà de 10 | Aucun | 5 minutes |
| `monitorflow_mysql_replica_io_running` (sur un réplica) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_mysql_replica_sql_running` (sur un réplica) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_mysql_replication_delay_seconds` (sur un réplica) | Au-delà de 300 (5 minutes) | Au-delà de 1800 (30 minutes) | 5 minutes |

Les alertes de réplication restent muettes sur un serveur qui n'est pas un réplica : la métrique n'existe pas.

### Vérifications de service et de port

Deux vérifications plus simples gardent leur place, par exemple sur un serveur où vous ne voulez pas de compte dans la base :

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « MySQL or MariaDB running » (Linux) | Le service systemd `mariadb` est actif. Métrique `service_mariadb_active` : 1 actif, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « MySQL or MariaDB answers » (Linux et Windows) | Le port 3306 accepte une connexion TCP sur 127.0.0.1. Métrique `port_3306_up` | Critique en cas d'échec, pendant 1 minute |

**Choisissez le bon nom d'unité :** `mariadb` pour MariaDB, `mysql` pour MySQL sous Debian et Ubuntu, `mysqld` pour MySQL sous RHEL et Rocky. La métrique suit : avec `mysql`, c'est `service_mysql_active`. Un service absent compte comme arrêté : un mauvais nom déclenche une alerte au lieu de masquer le problème.

Une vérification de port prouve que le serveur accepte les connexions TCP, pas qu'il répond aux requêtes : le contrôle MySQL ou MariaDB, lui, le vérifie. Pour le reste, appelez `mysqladmin ping` ou votre propre requête depuis un script. Voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

L'espace disque est vérifié sur chaque serveur sans configuration : avertissement à 85 % de remplissage, critique à 95 %, avant que les journaux binaires ne remplissent le disque.

## Créer un compte en lecture seule

`PROCESS` et `REPLICATION CLIENT` suffisent au contrôle. Ils lui permettent de lire l'état et la réplication, pas vos données. Quand l'agent fait tourner le contrôle sur le serveur de bases, créez le compte pour `localhost` :

```sql
CREATE USER 'monitorflow'@'localhost' IDENTIFIED BY 'choisissez-un-long-mot-de-passe';
GRANT PROCESS, REPLICATION CLIENT ON *.* TO 'monitorflow'@'localhost';
```

Le contrôle se connecte en TCP à `127.0.0.1` : si le serveur tourne avec `skip-name-resolve`, créez plutôt le compte pour `'127.0.0.1'`. Quand le serveur central ou un relais le fait tourner, indiquez le réseau d'où ils se connectent, par exemple `'monitorflow'@'10.0.0.%'`.

La taille des schémas ne couvre que ceux que le compte voit. Pour en inclure un, vous pouvez ajouter `GRANT SELECT ON shop.* TO 'monitorflow'@'localhost';`. C'est facultatif, et cela permet au compte de lire les données de ce schéma.

**MySQL 8 et le chiffrement :** gardez « Chiffrement » à sa valeur par défaut. Avec « Aucun », la connexion habituelle de MySQL 8 demande un échange de clé RSA que le contrôle ne prend pas en charge : le contrôle échoue. MySQL 8 propose TLS d'origine, la valeur par défaut fonctionne donc.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `MariaDB` ou `MySQL` dans « Find a check ».
3. Choisissez « MySQL ou MariaDB », le « Contrôle d’application ».
4. Gardez `127.0.0.1` dans « Adresse » et `3306` dans « Port » si la base tourne sur ce serveur. Gardez `monitorflow` dans « Nom d’utilisateur » et saisissez le mot de passe du compte dans « Mot de passe ».
5. Laissez « Ajouter les alertes recommandées » activé, puis cliquez sur « Add check ».

Le mot de passe est gardé scellé sur le serveur central, chiffré comme ses autres secrets. Il n'est jamais réaffiché, jamais écrit dans un journal, et l'API indique seulement s'il est défini. Si vous changez plus tard « Adresse » ou « Port », saisissez de nouveau le mot de passe.

Sous « Plus d’options », « Chiffrement » vaut « Quand le serveur le propose » (par défaut : chiffré si le serveur le propose, certificat non vérifié), « Obligatoire, certificat vérifié » ou « Aucun ».

Le contrôle tourne toutes les 60 secondes par défaut. Pour surveiller d'un coup tous les serveurs de bases d'un site, choisissez « Every server of » suivi du site sous « Add to », ou ajoutez-le depuis « Settings › Sites », « Alerts and checks », si tous les serveurs du site font tourner la base avec le même compte.

### Où tourne le contrôle

« Vérifié depuis » choisit qui se connecte :

- « L’agent de ce serveur » (par défaut) : l'agent du serveur de bases se connecte à `127.0.0.1`. Si son administrateur a défini des cibles de ports (`sensors.port_targets` dans `agent.toml`), l'adresse et le port doivent en faire partie.
- « Le serveur central », ou « Relais » suivi du nom d'un relais du site : pour la vérifier de l'extérieur, comme les applications l'atteignent. Saisissez l'adresse du serveur dans « Adresse », faites écouter la base sur cette adresse (`bind-address`, et pas de `skip-networking`), et créez le compte pour l'adresse du serveur central ou du relais. Le serveur central ne se connecte jamais aux adresses link-local (`169.254.x.x`, `fe80::`).

Un équipement, une machine sans agent, a le même contrôle dans son catalogue : il tourne toujours depuis le serveur central ou un relais, vers l'adresse de l'équipement.

### Par l'API

L'API publique ajoute le même contrôle : `POST /api/v1/servers/{id}/sensor_configs` avec le type, ses paramètres, et `default_alerts` pour ajouter les alertes recommandées. `GET /api/v1/check-templates` liste les métriques et alertes de chaque type.

```json
{
  "kind": "mysql",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 3306, "user": "monitorflow", "password": "choisissez-un-long-mot-de-passe" },
  "default_alerts": true
}
```

### Vérifications de service et de port

1. Dans « Find a check », tapez `MariaDB` ou `MySQL` et choisissez « MySQL or MariaDB running ». Dans « Service », gardez `mariadb` ou saisissez `mysql` ou `mysqld`.
2. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
3. Faites de même avec « MySQL or MariaDB answers ». Si le serveur n'écoute que sur une autre adresse (`bind-address`), saisissez-la dans « Address ».

Ne les ajoutez à tout un site que si tous ses serveurs font tourner la base : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », les alertes recommandées sont des alertes ordinaires, une ligne chacune sous « When to alert ». Ouvrez le menu d'une ligne et choisissez « Edit » : « Alert » règle critique ou avertissement, « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite ; plus long convient à une base lente à redémarrer après les mises à jour), « Apply to » ce serveur ou tous ceux du site.

Les vérifications de service et de port se règlent de la même façon : ouvrez le menu de la vérification et choisissez « Edit ». Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: mysql_up is 0 on db-2 (lyon)
Warning: mysql_slow_queries is 24 on db-2 (lyon)
Critical: service_mariadb_active mariadb.service is 0 on db-2 (lyon)
Critical: port_3306_up is 0 on db-2 (lyon)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand la base revient.

Quand `mysql_up` vaut 0, son étiquette `reason` dit pourquoi, et son étiquette `error` donne les mots du serveur :

| `reason` | Que vérifier |
| --- | --- |
| `connect` | Rien ne répond à cette adresse et ce port : le serveur est arrêté, écoute ailleurs (`bind-address`, `port`, `skip-networking`), ou un pare-feu refuse la connexion |
| `timeout` | Pas de réponse à temps : un pare-feu qui jette les paquets, ou un serveur trop chargé pour répondre |
| `tls` | Le chiffrement a échoué : avec « Obligatoire, certificat vérifié », le certificat doit être valide pour le nom saisi dans « Adresse » |
| `login` | Mauvais nom d'utilisateur ou mot de passe, ou la partie hôte du compte ne correspond pas à l'endroit d'où se connecte le contrôle |
| `permission` | Il manque `PROCESS` ou `REPLICATION CLIENT` au compte |
| `protocol` | Autre chose a répondu sur ce port, ou le serveur a demandé une connexion que le contrôle ne prend pas en charge (voir MySQL 8 et le chiffrement plus haut) |

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La cause de l'arrêt :** `systemctl status mariadb` et `journalctl -u mariadb -n 50` (ou votre unité). L'onglet « Logs » du serveur affiche aussi les avertissements et erreurs du journal.
2. **L'espace disque :** `df -h`. Journaux binaires et grosses tables temporaires remplissent vite un volume.
3. **La mémoire :** `journalctl -k | grep -i oom` montre si le noyau a tué `mysqld` ou `mariadbd`.
4. **Le service tourne mais le port échoue :** vérifiez `bind-address`, `port` et `skip-networking`, et le pare-feu local.
5. **Connexions ou requêtes lentes :** `SHOW FULL PROCESSLIST;` montre qui est connecté et ce qui tourne.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
