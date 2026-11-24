---
title: "Comment surveiller Redis"
description: "Suivez la mémoire, les évictions, les clients, les sauvegardes et la réplication de Redis, et soyez alerté quand il ne répond plus, avec le contrôle Redis prêt à l'emploi de MonitorFlow."
slug: surveiller-redis
lang: fr
translation: monitor-redis
---

# Comment surveiller Redis

Redis travaille souvent en silence derrière une application : cache, sessions ou file de tâches. Quand il s'arrête, les connexions échouent, les files se figent et les pages ralentissent, sans explication. MonitorFlow demande à Redis comment il va chaque minute, et vous prévient en une à deux minutes quand il ne répond plus, manque de mémoire ou n'arrive plus à sauvegarder.

## Pourquoi c'est important

Redis garde ses données en mémoire : rapide, mais fragile. Il s'arrête quand le noyau le tue faute de mémoire, quand un instantané ne peut pas être écrit sur un disque plein, ou après une mise à jour ou un redémarrage qui ne l'a pas relancé. Les applications traitent souvent un cache absent comme un chemin lent plutôt qu'une erreur : la panne peut passer inaperçue des heures.

Avant de s'arrêter, il prévient souvent : la mémoire atteint `maxmemory` et des clés sont évincées, des clients sont refusés, le dernier instantané a échoué, un réplica a perdu son primaire. Redis indique tout cela dans `INFO`.

## Ce que MonitorFlow vérifie

### Le contrôle Redis

L'entrée « Redis » du catalogue (un « Contrôle d’application ») fonctionne avec Redis 5 ou plus récent, et les serveurs qui parlent son protocole comme Valkey et KeyDB. Chaque minute, il lance `INFO` et lit :

- s'il répond et si la connexion du compte réussit, et en combien de temps ;
- les clients connectés, ceux bloqués sur une commande, et les connexions refusées pour `maxclients` ;
- la mémoire utilisée par rapport à `maxmemory` (quand il est défini), la fragmentation, et les clés évincées ;
- le taux de succès des lectures de clés et le nombre de clés ;
- si le dernier instantané (RDB) et la dernière écriture du fichier append-only (AOF) ont réussi ;
- les réplicas connectés et leur retard ; sur un réplica, s'il est relié à son primaire.

Les métriques portent le nom du contrôle : `monitorflow_redis_up`, `monitorflow_redis_memory_usage_ratio`, etc. (avec un autre nom de contrôle, ce nom remplace `redis`).

Avec « Ajouter les alertes recommandées » activé, ces alertes l'accompagnent :

| Métrique | Avertissement | Critique | Pendant |
| --- | --- | --- | --- |
| `monitorflow_redis_up` (il répond et la connexion réussit) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_redis_memory_usage_ratio` (avec `maxmemory`) | Au-delà de 0,8 (80 %) | Au-delà de 0,95 | 5 minutes |
| `monitorflow_redis_evicted_keys` (depuis le contrôle précédent) | Au-delà de 0 | Aucun | 15 minutes |
| `monitorflow_redis_rejected_connections` (depuis le contrôle précédent) | Au-delà de 0 | Aucun | Tout de suite |
| `monitorflow_redis_last_save_ok` | Aucun | Sous 1 | Tout de suite |
| `monitorflow_redis_aof_write_ok` (avec AOF) | Aucun | Sous 1 | Tout de suite |
| `monitorflow_redis_primary_link_up` (sur un réplica) | Aucun | Sous 1 | 2 minutes |

Une alerte dont la métrique n'existe pas reste muette : sans `maxmemory`, AOF ni réplication, ces lignes ne se déclenchent jamais.

### Vérifications de service et de port

Deux vérifications plus simples gardent leur place :

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « Redis running » | Le service systemd `redis-server` est actif. Métrique `service_redis_server_active` : 1 actif, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « Redis answers » | Le port 6379 accepte une connexion TCP sur 127.0.0.1. Métrique `port_6379_up` | Critique en cas d'échec, pendant 1 minute |

L'unité s'appelle `redis-server` sous Debian et Ubuntu, `redis` sous RHEL et Rocky, où la métrique devient `service_redis_active`. Un service absent compte comme arrêté : un mauvais nom déclenche une alerte.

Une vérification de port prouve que Redis accepte les connexions TCP, pas qu'il répond aux commandes : le contrôle Redis, lui, le vérifie. Pour le reste, voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

La mémoire est vérifiée sur chaque serveur sans configuration : avertissement sous 10 % disponible, critique sous 5 %, pendant 5 minutes. Sur un serveur Redis, c'est souvent le premier signe d'ennui.

**Redis chiffré n'est pas encore vérifié.** Si Redis n'accepte que des connexions TLS, gardez pour l'instant les vérifications de service et de port.

## Lui donner un compte

Le contrôle n'a besoin que de `INFO` et `PING`. Sous Redis 6 ou plus récent, créez un utilisateur ACL qui peut lancer ces deux commandes et rien d'autre :

```
ACL SETUSER monitorflow on >choisissez-un-long-mot-de-passe -@all +info +ping
```

Enregistrez-le avec `ACL SAVE` si vous utilisez un fichier ACL, sinon avec `CONFIG REWRITE`, pour qu'il survive à un redémarrage.

Sous un Redis plus ancien, ou sans utilisateurs ACL, laissez « Nom d’utilisateur » vide et saisissez le mot de passe `requirepass`. Un Redis sans mot de passe ne demande ni l'un ni l'autre.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `Redis` dans « Find a check ».
3. Choisissez « Redis », le « Contrôle d’application ».
4. Gardez `127.0.0.1` dans « Adresse » et `6379` dans « Port » si Redis tourne sur ce serveur. Saisissez `monitorflow` dans « Nom d’utilisateur » et son mot de passe dans « Mot de passe », ou seulement le mot de passe, ou rien.
5. Laissez « Ajouter les alertes recommandées » activé, puis cliquez sur « Add check ».

Le mot de passe est gardé scellé sur le serveur central, chiffré comme ses autres secrets. Il n'est jamais réaffiché, jamais écrit dans un journal, et l'API indique seulement s'il est défini. Si vous changez plus tard « Adresse » ou « Port », saisissez de nouveau le mot de passe.

Le contrôle tourne toutes les 60 secondes par défaut. Pour surveiller d'un coup tous les serveurs Redis d'un site, choisissez « Every server of » suivi du site sous « Add to », ou ajoutez-le depuis « Settings › Sites », « Alerts and checks », si tous les serveurs du site font tourner Redis avec le même compte.

### Où tourne le contrôle

« Vérifié depuis » choisit qui se connecte :

- « L’agent de ce serveur » (par défaut) : l'agent du serveur se connecte à `127.0.0.1`. Si son administrateur a défini des cibles de ports (`sensors.port_targets` dans `agent.toml`), l'adresse et le port doivent en faire partie.
- « Le serveur central », ou « Relais » suivi du nom d'un relais du site : pour le vérifier de l'extérieur, comme les applications l'atteignent. Saisissez l'adresse du serveur dans « Adresse », faites écouter Redis sur cette adresse (`bind` dans `redis.conf`), et donnez-lui un mot de passe : sans mot de passe, le `protected-mode` de Redis refuse les connexions des autres machines. Autorisez l'adresse du serveur central ou du relais dans le pare-feu. Le serveur central ne se connecte jamais aux adresses link-local (`169.254.x.x`, `fe80::`).

Un équipement, une machine sans agent, a le même contrôle dans son catalogue : il tourne toujours depuis le serveur central ou un relais, vers l'adresse de l'équipement.

### Par l'API

L'API publique ajoute le même contrôle : `POST /api/v1/servers/{id}/sensor_configs` avec le type, ses paramètres, et `default_alerts` pour ajouter les alertes recommandées. `GET /api/v1/check-templates` liste les métriques et alertes de chaque type.

```json
{
  "kind": "redis",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 6379, "user": "monitorflow", "password": "choisissez-un-long-mot-de-passe" },
  "default_alerts": true
}
```

### Vérifications de service et de port

1. Dans « Find a check », tapez `Redis` et choisissez « Redis running ». Dans « Service », gardez `redis-server` ou saisissez `redis`.
2. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
3. Faites de même avec « Redis answers ». Si Redis n'écoute que sur une autre adresse (`bind` dans `redis.conf`), saisissez-la dans « Address ».

Ne les ajoutez à tout un site que si tous ses serveurs font tourner Redis : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », les alertes recommandées sont des alertes ordinaires, une ligne chacune sous « When to alert ». Ouvrez le menu d'une ligne et choisissez « Edit » : « Alert » règle critique ou avertissement (un cache qui ne fait que ralentir peut mériter un avertissement), « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite), « Apply to » ce serveur ou tous ceux du site. Un pur cache qui évince des clés exprès peut se passer de l'alerte d'éviction : ouvrez son menu et désactivez-la.

Les vérifications de service et de port se règlent de la même façon : ouvrez le menu de la vérification et choisissez « Edit ». Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: redis_up is 0 on cache-1 (paris)
Critical: redis_last_save_ok is 0 on cache-1 (paris)
Critical: service_redis_server_active redis-server.service is 0 on cache-1 (paris)
Critical: port_6379_up is 0 on cache-1 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand Redis revient.

Quand `redis_up` vaut 0, son étiquette `reason` dit pourquoi, et son étiquette `error` donne les mots de Redis :

| `reason` | Que vérifier |
| --- | --- |
| `connect` | Rien ne répond à cette adresse et ce port : Redis est arrêté, écoute ailleurs (`bind`, `port`), ou un pare-feu refuse la connexion |
| `timeout` | Pas de réponse à temps : un pare-feu qui jette les paquets, ou un Redis trop occupé pour répondre |
| `login` | Mauvais nom d'utilisateur ou mot de passe, ou Redis demande un mot de passe et aucun n'est saisi (`NOAUTH`) |
| `permission` | L'utilisateur ACL ne peut pas lancer `INFO` ou `PING` (`NOPERM`) |
| `protocol` | Autre chose a répondu sur ce port, ou Redis a renvoyé une erreur, comme le `protected-mode` qui refuse une connexion d'une autre machine |

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La cause de l'arrêt :** `systemctl status redis-server` et `journalctl -u redis-server -n 50`. L'onglet « Logs » du serveur affiche aussi les avertissements et erreurs du journal.
2. **La mémoire :** `journalctl -k | grep -i oom` montre si le noyau l'a tué. Vérifiez `maxmemory` et `maxmemory-policy` dans `redis.conf`.
3. **L'espace disque :** `df -h`. Un instantané raté sur un disque plein peut amener Redis à refuser les écritures ; `redis-cli INFO persistence` montre la dernière erreur.
4. **Le service tourne mais le port échoue :** vérifiez `bind` et `port` dans `redis.conf`, et le pare-feu local.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
