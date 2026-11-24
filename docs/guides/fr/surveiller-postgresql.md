---
title: "Comment surveiller PostgreSQL"
description: "Suivez les connexions, transactions longues, verrous, cache et réplication de PostgreSQL, et soyez alerté quand il ne répond plus, avec le contrôle PostgreSQL prêt à l'emploi de MonitorFlow."
slug: surveiller-postgresql
lang: fr
translation: monitor-postgresql
---

# Comment surveiller PostgreSQL

Quand PostgreSQL s'arrête, les applications qui en dépendent s'arrêtent aussi, et le premier signe est souvent une page d'erreur vue par un autre. MonitorFlow se connecte à PostgreSQL avec un compte en lecture seule, lit son état, et vous prévient en une à deux minutes quand il ne répond plus ou commence à peiner.

## Pourquoi c'est important

PostgreSQL plante rarement seul. Il s'arrête à cause de son environnement : disque rempli de fichiers WAL, noyau qui le tue faute de mémoire, mise à jour qui l'a relancé avec une configuration invalide, cluster non démarré après un redémarrage. L'application consigne des erreurs de connexion que personne ne lit, et vous l'apprenez par un utilisateur.

Avant de s'arrêter, il ralentit souvent : les connexions s'épuisent, une transaction restée ouverte bloque le nettoyage, des requêtes attendent des verrous, un réplica prend des heures de retard. Les statistiques de PostgreSQL le montrent bien avant que les utilisateurs se plaignent.

## Ce que MonitorFlow vérifie

### Le contrôle PostgreSQL

L'entrée « PostgreSQL » du catalogue (un « Contrôle d’application ») se connecte à PostgreSQL chaque minute, s'authentifie, et lit :

- s'il répond et si la connexion du compte réussit, et en combien de temps ;
- les connexions par rapport à `max_connections`, celles qui exécutent une requête, et celles inactives dans une transaction ouverte ;
- les requêtes lentes (plus de 30 secondes par défaut), la requête la plus longue et la plus ancienne transaction ouverte ;
- les verrous attendus, les interblocages, le taux de succès du cache et les transactions par seconde ;
- la taille de chaque base ;
- sur un primaire, ses réplicas et le retard de chacun ; sur un réplica, s'il reçoit le flux de son primaire et l'âge de la dernière transaction rejouée.

Les métriques portent le nom du contrôle : `monitorflow_postgresql_up`, `monitorflow_postgresql_connections_ratio`, etc. (avec un autre nom de contrôle, ce nom remplace `postgresql`).

Avec « Ajouter les alertes recommandées » activé, ces alertes l'accompagnent :

| Métrique | Avertissement | Critique | Pendant |
| --- | --- | --- | --- |
| `monitorflow_postgresql_up` (il répond et la connexion réussit) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_postgresql_connections_ratio` | Au-delà de 0,8 (80 %) | Au-delà de 0,95 | 5 minutes |
| `monitorflow_postgresql_longest_transaction_seconds` | Au-delà de 3600 (1 heure) | Aucun | 5 minutes |
| `monitorflow_postgresql_locks_waiting` | Au-delà de 10 | Aucun | 5 minutes |
| `monitorflow_postgresql_deadlocks` | Au-delà de 0 | Aucun | Tout de suite |
| `monitorflow_postgresql_cache_hit_ratio` | Sous 0,9 | Aucun | 30 minutes |
| `monitorflow_postgresql_replica_lag_bytes` (par réplica) | Au-delà de 1 Gio | Aucun | 10 minutes |
| `monitorflow_postgresql_wal_receiver_up` (sur un réplica) | Aucun | Sous 1 | 5 minutes |
| `monitorflow_postgresql_replication_delay_seconds` (sur un réplica) | Au-delà de 300 (5 minutes) | Au-delà de 1800 (30 minutes) | 5 minutes |

Les alertes de réplication restent muettes sur un serveur sans réplication : la métrique n'existe pas.

### Vérifications de service et de port

Deux vérifications plus simples gardent leur place, par exemple sur un serveur où vous ne voulez pas de compte dans la base :

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « PostgreSQL running » | Le service systemd `postgresql` est actif. Métrique `service_postgresql_active` : 1 actif, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « PostgreSQL answers » | Le port 5432 accepte une connexion TCP sur 127.0.0.1. Métrique `port_5432_up` | Critique en cas d'échec, pendant 1 minute |

Un service absent compte comme arrêté : une faute de frappe dans le nom de l'unité déclenche une alerte au lieu de passer inaperçue.

**Sous Debian et Ubuntu, vérifiez l'unité du cluster.** `postgresql.service` y est une unité parapluie qui reste active même base arrêtée. Le serveur tourne sous `postgresql@<version>-<cluster>`, par exemple `postgresql@16-main` (`systemctl list-units 'postgresql@*'` affiche le vôtre) : saisissez ce nom. Sous RHEL et Rocky, `postgresql` convient (`postgresql-16` avec les paquets de la communauté).

Une vérification de port prouve que PostgreSQL accepte les connexions TCP, pas qu'il répond aux requêtes : le contrôle PostgreSQL, lui, le vérifie. Pour le reste, appelez `pg_isready` ou votre propre requête depuis un script. Voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

L'espace disque est vérifié sur chaque serveur sans configuration : avertissement à 85 % de remplissage, critique à 95 %.

## Créer un compte en lecture seule

Le contrôle demande PostgreSQL 10 ou plus récent et un compte qui peut se connecter. Donnez-lui le rôle `pg_monitor`, et rien d'autre :

```sql
CREATE ROLE monitorflow LOGIN PASSWORD 'choisissez-un-long-mot-de-passe';
GRANT pg_monitor TO monitorflow;
```

`pg_monitor` lui permet de voir toutes les sessions, la réplication et la taille des bases, sans lire ni modifier vos données. Sans lui, le contrôle fonctionne, mais PostgreSQL cache ce qu'exécutent les autres rôles : les chiffres ne couvrent que les sessions du contrôle.

Le compte doit aussi être autorisé dans `pg_hba.conf` depuis l'endroit où tourne le contrôle. Sur le serveur lui-même, la ligne habituelle `host all all 127.0.0.1/32 scram-sha-256` suffit. Rechargez PostgreSQL après une modification (`systemctl reload` avec votre unité).

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `PostgreSQL` dans « Find a check ».
3. Choisissez « PostgreSQL », le « Contrôle d’application ».
4. Gardez `127.0.0.1` dans « Adresse » et `5432` dans « Port » si PostgreSQL tourne sur ce serveur. Gardez `monitorflow` dans « Nom d’utilisateur » et saisissez le mot de passe du compte dans « Mot de passe ».
5. Laissez « Ajouter les alertes recommandées » activé, puis cliquez sur « Add check ».

Le mot de passe est gardé scellé sur le serveur central, chiffré comme ses autres secrets. Il n'est jamais réaffiché, jamais écrit dans un journal, et l'API indique seulement s'il est défini. Si vous changez plus tard « Adresse » ou « Port », saisissez de nouveau le mot de passe.

« Plus d’options » contient le reste :

- « Base de données » : la base à laquelle se connecter, `postgres` par défaut.
- « Chiffrement » : « Quand le serveur le propose » (par défaut : chiffré si PostgreSQL le propose, certificat non vérifié), « Obligatoire, certificat vérifié » ou « Aucun ».
- « Lente après (secondes) » : une requête qui dure plus longtemps compte comme lente, 30 par défaut.

Le contrôle tourne toutes les 60 secondes par défaut. Pour surveiller d'un coup tous les serveurs de bases d'un site, choisissez « Every server of » suivi du site sous « Add to », ou ajoutez-le depuis « Settings › Sites », « Alerts and checks », si tous les serveurs du site font tourner PostgreSQL avec le même compte.

### Où tourne le contrôle

« Vérifié depuis » choisit qui se connecte :

- « L’agent de ce serveur » (par défaut) : l'agent du serveur de bases se connecte à `127.0.0.1`. Si son administrateur a défini des cibles de ports (`sensors.port_targets` dans `agent.toml`), l'adresse et le port doivent en faire partie.
- « Le serveur central », ou « Relais » suivi du nom d'un relais du site : pour le vérifier de l'extérieur, comme les applications l'atteignent. Saisissez l'adresse du serveur dans « Adresse », faites écouter PostgreSQL sur cette adresse (`listen_addresses` dans `postgresql.conf`), et autorisez l'adresse du serveur central ou du relais dans `pg_hba.conf`, par exemple `host all monitorflow 10.0.0.5/32 scram-sha-256`. Le serveur central ne se connecte jamais aux adresses link-local (`169.254.x.x`, `fe80::`).

Un équipement, une machine sans agent, a le même contrôle dans son catalogue : il tourne toujours depuis le serveur central ou un relais, vers l'adresse de l'équipement.

### Par l'API

L'API publique ajoute le même contrôle : `POST /api/v1/servers/{id}/sensor_configs` avec le type, ses paramètres, et `default_alerts` pour ajouter les alertes recommandées. `GET /api/v1/check-templates` liste les métriques et alertes de chaque type.

```json
{
  "kind": "postgresql",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 5432, "user": "monitorflow", "password": "choisissez-un-long-mot-de-passe" },
  "default_alerts": true
}
```

### Vérifications de service et de port

1. Dans « Find a check », tapez `PostgreSQL` et choisissez « PostgreSQL running ». Dans « Service », gardez `postgresql` ou saisissez l'unité de votre cluster.
2. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
3. Faites de même avec « PostgreSQL answers ». Si PostgreSQL n'écoute que sur une autre adresse, saisissez-la dans « Address ».

Ne les ajoutez à tout un site que si tous ses serveurs font tourner PostgreSQL : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », les alertes recommandées sont des alertes ordinaires, une ligne chacune sous « When to alert ». Ouvrez le menu d'une ligne et choisissez « Edit » : « Alert » règle critique ou avertissement, « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite), « Apply to » ce serveur ou tous ceux du site. Un serveur chargé peut demander un seuil de connexions plus haut ; une base de rapports peut garder des transactions longues exprès.

Les vérifications de service et de port se règlent de la même façon : ouvrez le menu de la vérification et choisissez « Edit ». Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: postgresql_up is 0 on db-1 (paris)
Warning: postgresql_connections_ratio is 0.86 on db-1 (paris)
Critical: service_postgresql_active postgresql.service is 0 on db-1 (paris)
Critical: port_5432_up is 0 on db-1 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand PostgreSQL revient.

Quand `postgresql_up` vaut 0, son étiquette `reason` dit pourquoi, et son étiquette `error` donne les mots de PostgreSQL :

| `reason` | Que vérifier |
| --- | --- |
| `connect` | Rien ne répond à cette adresse et ce port : PostgreSQL est arrêté, écoute ailleurs (`listen_addresses`, `port`), ou un pare-feu refuse la connexion |
| `timeout` | Pas de réponse à temps : un pare-feu qui jette les paquets, ou un serveur trop chargé pour répondre |
| `tls` | Le chiffrement a échoué : avec « Obligatoire, certificat vérifié », le certificat doit être valide pour le nom saisi dans « Adresse » |
| `login` | Mauvais nom d'utilisateur ou mot de passe, ou aucune ligne de `pg_hba.conf` n'autorise le compte depuis l'endroit où tourne le contrôle |
| `permission` | Il manque un droit au compte, ou la base saisie dans « Base de données » n'existe pas |
| `protocol` | Autre chose a répondu sur ce port, ou PostgreSQL a répondu de façon inattendue |

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La cause de l'arrêt :** `systemctl status` et `journalctl -u` avec votre unité, par exemple `postgresql@16-main`. L'onglet « Logs » du serveur affiche aussi les avertissements et erreurs du journal.
2. **L'espace disque :** un volume de données ou de WAL plein arrête PostgreSQL. Lancez `df -h`.
3. **La mémoire :** `journalctl -k | grep -i oom` montre si le noyau l'a tué.
4. **Le service tourne mais le port échoue :** vérifiez `listen_addresses` et `port` dans `postgresql.conf`, et le pare-feu local.
5. **Connexions ou transactions longues :** `SELECT pid, usename, state, xact_start, query FROM pg_stat_activity ORDER BY xact_start;` montre qui les tient.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
