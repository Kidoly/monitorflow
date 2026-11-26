---
title: "Comment surveiller SQL Server"
description: "Suivez les connexions, lots de requêtes, la durée de vie des pages, les blocages, la taille des bases et des journaux et les réplicas Always On de SQL Server, et soyez alerté quand il ne répond plus, avec le contrôle prêt à l'emploi de MonitorFlow."
slug: surveiller-sql-server
lang: fr
translation: monitor-sql-server
---

# Comment surveiller SQL Server

Un ERP, un logiciel de comptabilité ou une application métier dépend souvent d'une seule instance SQL Server sur un seul serveur Windows. Quand elle s'arrête ou ralentit, tous ses utilisateurs sont bloqués. MonitorFlow se connecte avec un identifiant qui ne peut que lire l'état du serveur, et vous prévient en une à deux minutes quand SQL Server ne répond plus ou commence à peiner.

## Pourquoi c'est important

SQL Server s'arrête après une mise à jour Windows qui a redémarré le serveur en laissant le service arrêté, quand un lecteur de données ou de journaux se remplit, quand le mot de passe du compte de service a changé, ou après un plantage. Les utilisateurs appellent : l'application « ne s'ouvre pas », et l'on cherche longtemps quel serveur est en cause.

Les ennuis commencent souvent plus tôt : une session garde un verrou et les autres attendent derrière elle, le buffer pool est trop petit et les pages quittent la mémoire en quelques secondes, un journal de transactions ne peut plus grandir faute de place, un secondaire Always On prend du retard. Les compteurs de SQL Server le montrent en premier.

## Ce que MonitorFlow vérifie

### Le contrôle SQL Server

L'entrée « SQL Server » du catalogue (un « Contrôle d’application ») fonctionne avec SQL Server 2012 ou plus récent, sous Windows ou Linux. Chaque minute, il se connecte et lit :

- s'il répond et si la connexion réussit, et en combien de temps ;
- les connexions utilisateur par rapport au réglage `user connections` (32 767 quand il n'y a pas de limite) ;
- les lots de requêtes par seconde, la durée de vie des pages (page life expectancy) et le taux de succès du cache ;
- les requêtes bloquées par le verrou d'une autre, la plus longue attente parmi elles, et la requête la plus longue en cours ;
- les attentes de verrous par seconde et les interblocages ;
- la taille des fichiers de données et du journal de transactions de chaque base, et le remplissage de chaque journal ;
- avec Always On, la connexion et la santé de chaque réplica, et pour chaque base d'un secondaire, le journal pas encore envoyé, le journal pas encore rejoué et son retard (SQL Server 2016 ou plus récent).

Les métriques portent le nom du contrôle : `monitorflow_mssql_up`, `monitorflow_mssql_page_life_expectancy_seconds`, etc. (avec un autre nom de contrôle, ce nom remplace `mssql`). Une base est l'instance de ses métriques (`shop`), un réplica `groupe/serveur` et une base sur un réplica `groupe/serveur/base`.

Avec « Ajouter les alertes recommandées » activé, ces alertes l'accompagnent :

| Métrique | Avertissement | Critique | Pendant |
| --- | --- | --- | --- |
| `monitorflow_mssql_up` (il répond et la connexion réussit) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_mssql_connections_ratio` | Au-delà de 0,8 (80 %) | Au-delà de 0,95 | 5 minutes |
| `monitorflow_mssql_page_life_expectancy_seconds` | Sous 300 | Aucun | 15 minutes |
| `monitorflow_mssql_buffer_cache_hit_ratio` | Sous 0,95 | Aucun | 30 minutes |
| `monitorflow_mssql_processes_blocked` | Au-delà de 0 | Aucun | 5 minutes |
| `monitorflow_mssql_deadlocks` (depuis la dernière vérification) | Au-delà de 0 | Aucun | Tout de suite |
| `monitorflow_mssql_log_used_ratio` (par base) | Au-delà de 0,8 | Au-delà de 0,95 | 5 minutes |
| `monitorflow_mssql_ag_replica_connected` (par réplica) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_mssql_ag_replica_healthy` (par réplica) | Aucun | Sous 1 | 5 minutes |
| `monitorflow_mssql_ag_log_send_queue_bytes` (par base d'un secondaire) | Au-delà de 1 Gio | Aucun | 10 minutes |
| `monitorflow_mssql_ag_secondary_lag_seconds` (par base d'un secondaire) | Au-delà de 300 (5 minutes) | Au-delà de 1800 (30 minutes) | 5 minutes |

Les alertes Always On restent muettes sur un serveur sans groupe de disponibilité : les métriques n'existent pas. Une durée de vie des pages de 300 secondes est le plancher habituel d'un petit serveur ; sur un serveur doté de beaucoup de mémoire, relevez-le (une règle courante : 300 par tranche de 4 Go de buffer pool).

### Vérifications de service et de port

Deux vérifications plus simples gardent leur place, par exemple sur un serveur où vous ne voulez pas d'identifiant SQL Server :

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « SQL Server actif » | Le service Windows `MSSQLSERVER`, l'instance par défaut, est démarré. Métrique `service_mssqlserver_active` : 1 démarré, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « SQL Server répond » | Le port 1433 accepte une connexion TCP sur 127.0.0.1. Métrique `port_1433_up` | Critique en cas d'échec, pendant 1 minute |

**Les instances nommées** tournent sous `MSSQL$` suivi du nom de l'instance, par exemple `MSSQL$SQLEXPRESS` : saisissez le nom du service, pas son nom d'affichage. Un service absent compte comme arrêté : un mauvais nom déclenche une alerte. Pour l'Agent SQL Server, ajoutez « Un service Windows tourne » avec `SQLSERVERAGENT` (ou `SQLAgent$` suivi du nom de l'instance). Sous Linux, utilisez « Un service tourne » avec `mssql-server`.

SQL Server écrit ses erreurs dans le journal d'événements Application : « Erreurs du journal d’événements Application » lève un avertissement à chaque nouvel événement d'erreur ou critique. L'espace disque de chaque lecteur est vérifié sans configuration.

## Créer un identifiant qui ne lit que l'état du serveur

Le contrôle demande un identifiant SQL Server avec un seul droit : `VIEW SERVER STATE`, ou `VIEW SERVER PERFORMANCE STATE` à partir de SQL Server 2022. Il permet de lire les compteurs et les sessions du serveur, pas vos données, et n'a besoin d'aucun utilisateur dans aucune base. Dans SQL Server Management Studio, ou avec `sqlcmd`, en administrateur :

```sql
CREATE LOGIN monitorflow WITH PASSWORD = 'choisissez-un-long-mot-de-passe';
GRANT VIEW SERVER STATE TO monitorflow;
-- SQL Server 2022 et plus récent : le droit plus restreint suffit.
-- GRANT VIEW SERVER PERFORMANCE STATE TO monitorflow;
```

Sur un groupe de disponibilité, créez l'identifiant sur chaque réplica : les identifiants ne sont pas copiés avec les bases.

**L'authentification SQL Server doit être activée.** Beaucoup de serveurs n'acceptent que les connexions Windows. Dans Management Studio, ouvrez « Propriétés › Sécurité » du serveur, choisissez « Mode d'authentification SQL Server et Windows », puis redémarrez le service. La connexion avec un compte Windows n'est pas encore prise en charge par le contrôle.

**TCP doit être activé, sur un port fixe.** SQL Server Express est livré avec TCP/IP désactivé, et une instance nommée choisit son port au démarrage : dans le Gestionnaire de configuration SQL Server, activez TCP/IP et donnez un port fixe à l'instance, puis saisissez ce port dans « Port ». Le contrôle ne demande pas à SQL Server Browser le port d'une instance nommée.

## Mise en place

1. Ouvrez « Fleet », cliquez sur la machine SQL Server et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `SQL Server` dans « Find a check ».
3. Choisissez « SQL Server », le « Contrôle d’application ».
4. Gardez `127.0.0.1` dans « Adresse » et `1433` dans « Port » si SQL Server tourne sur ce serveur à son port par défaut. Gardez `monitorflow` dans « Nom d’utilisateur » et saisissez le mot de passe de l'identifiant dans « Mot de passe ».
5. Laissez « Ajouter les alertes recommandées » activé, puis cliquez sur « Add check ».

Le mot de passe est gardé scellé sur le serveur central, chiffré comme ses autres secrets. Il n'est jamais réaffiché, jamais écrit dans un journal, et l'API indique seulement s'il est défini. Si vous changez plus tard « Adresse » ou « Port », saisissez de nouveau le mot de passe.

Dans « Plus d’options », « Chiffrement » vaut « Quand le serveur le propose » (par défaut : la session est chiffrée avec le certificat de SQL Server, même celui qu'il crée lui-même au démarrage, qui n'est pas vérifié), « Obligatoire, certificat vérifié » (le certificat doit être signé par une autorité de confiance de la machine, pour le nom saisi dans « Adresse ») ou « Aucun ». Avec « Aucun », le mot de passe traverse le réseau en clair : réservez-le à un contrôle sur le serveur lui-même, et seulement si SQL Server refuse le chiffrement. Quand le chiffrement est demandé et que SQL Server ne le propose pas, le contrôle échoue plutôt que d'envoyer le mot de passe en clair.

Le contrôle tourne toutes les 60 secondes par défaut. Pour surveiller d'un coup tous les serveurs SQL Server d'un site, choisissez « Every server of » suivi du site sous « Add to », si tous les serveurs du site font tourner SQL Server avec le même identifiant.

### Où tourne le contrôle

« Vérifié depuis » choisit qui se connecte :

- « L’agent de ce serveur » (par défaut) : l'agent de la machine SQL Server, Windows ou Linux, se connecte à `127.0.0.1`. Si son administrateur a défini des cibles de ports (`sensors.port_targets` dans `agent.toml`), l'adresse et le port doivent en faire partie.
- « Le serveur central », ou « Relais » suivi du nom d'un relais du site : pour le vérifier de l'extérieur, comme les applications l'atteignent. Saisissez l'adresse du serveur dans « Adresse », et laissez le pare-feu Windows accepter le port depuis le serveur central ou le relais. Le serveur central ne se connecte jamais aux adresses link-local (`169.254.x.x`, `fe80::`).

Un équipement, une machine sans agent comme une Azure SQL Managed Instance, a le même contrôle dans son catalogue : il tourne toujours depuis le serveur central ou un relais, vers l'adresse de l'équipement.

### Par l'API

L'API publique ajoute le même contrôle : `POST /api/v1/servers/{id}/sensor_configs` avec le type, ses paramètres, et `default_alerts` pour ajouter les alertes recommandées. `GET /api/v1/check-templates` liste les métriques et alertes de chaque type.

```json
{
  "kind": "mssql",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": "1433", "user": "monitorflow", "password": "choisissez-un-long-mot-de-passe" },
  "default_alerts": true
}
```

### Vérifications de service et de port

1. Dans « Find a check », tapez `SQL Server` et choisissez « SQL Server actif ». Gardez `MSSQLSERVER` dans « Service » ou saisissez votre instance nommée.
2. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
3. Faites de même avec « SQL Server répond », avec votre port s'il diffère de 1433.

Ne les ajoutez à tout un site que si tous ses serveurs font tourner SQL Server : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », les alertes recommandées sont des alertes ordinaires, une ligne chacune sous « When to alert ». Ouvrez le menu d'une ligne et choisissez « Edit » : « Alert » règle critique ou avertissement, « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite ; plus long évite une alerte quand Windows Update redémarre le serveur), « Apply to » ce serveur ou tous ceux du site. Un serveur qui fait tourner des traitements de nuit peut bloquer quelques minutes exprès : allongez « For at least » de `processes_blocked` plutôt que de la supprimer.

Les vérifications de service et de port se règlent de la même façon : ouvrez le menu de la vérification et choisissez « Edit ». Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: mssql_up is 0 on sql-1 (paris)
Warning: mssql_processes_blocked is 3 on sql-1 (paris)
Critical: mssql_log_used_ratio shop is 0.97 on sql-1 (paris)
Critical: service_mssqlserver_active MSSQLSERVER is 0 on sql-1 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand SQL Server revient.

Quand `mssql_up` vaut 0, son étiquette `reason` dit pourquoi, et son étiquette `error` donne les mots de SQL Server :

| `reason` | Que vérifier |
| --- | --- |
| `connect` | Rien ne répond à cette adresse et ce port : SQL Server est arrêté, TCP/IP est désactivé, l'instance écoute sur un autre port, ou un pare-feu refuse la connexion |
| `timeout` | Pas de réponse à temps : un pare-feu qui jette les paquets, ou un serveur trop chargé pour répondre |
| `tls` | Le chiffrement a échoué : SQL Server n'en propose pas alors que le contrôle le demande, ou l'exige alors que « Chiffrement » vaut « Aucun », ou avec « Obligatoire, certificat vérifié » son certificat n'est pas de confiance ou pas pour le nom saisi dans « Adresse » |
| `login` | Mauvais identifiant ou mot de passe (erreur 18456), authentification SQL Server désactivée, ou mot de passe de l'identifiant expiré |
| `permission` | Il manque `VIEW SERVER STATE` à l'identifiant (erreur 300) |
| `protocol` | Autre chose a répondu sur ce port, ou SQL Server a répondu d'une façon que le contrôle ne lit pas |

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **Le service :** `Get-Service MSSQLSERVER` dans PowerShell, puis `Start-Service MSSQLSERVER` s'il suffit de le démarrer.
2. **La cause de l'arrêt :** les journaux Application et Système dans l'Observateur d'événements, et le `ERRORLOG` de SQL Server dans le dossier `Log` de l'instance.
3. **L'espace disque :** un lecteur de données ou de journaux plein arrête les bases. L'onglet « Sensors » affiche chaque lecteur.
4. **Les blocages :** `SELECT session_id, blocking_session_id, wait_time, wait_type FROM sys.dm_exec_requests WHERE blocking_session_id <> 0;` montre qui attend et qui bloque.
5. **Un journal qui se remplit :** `SELECT name, log_reuse_wait_desc FROM sys.databases;` dit ce qui empêche de réutiliser le journal : une sauvegarde du journal qui ne tourne pas, une transaction longue, ou un réplica en retard.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web, choisissez Windows, et lancez la ligne affichée dans PowerShell en tant qu'administrateur sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md#windows) (en anglais).
