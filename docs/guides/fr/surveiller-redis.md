---
title: "Comment surveiller Redis"
description: "Soyez alerté quand Redis s'arrête ou n'accepte plus de connexions sur un serveur Linux, avec les vérifications de service et de port de MonitorFlow."
slug: surveiller-redis
lang: fr
translation: monitor-redis
---

# Comment surveiller Redis

Redis travaille souvent en silence derrière une application : cache, sessions ou file de tâches. Quand il s'arrête, les connexions échouent, les files se figent et les pages ralentissent, sans explication. MonitorFlow surveille le service Redis et son port, et vous prévient en une à deux minutes.

## Pourquoi c'est important

Redis garde ses données en mémoire : rapide, mais fragile. Il s'arrête quand le noyau le tue faute de mémoire, quand un instantané ne peut pas être écrit sur un disque plein, ou après une mise à jour ou un redémarrage qui ne l'a pas relancé. Les applications traitent souvent un cache absent comme un chemin lent plutôt qu'une erreur : la panne peut passer inaperçue des heures.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « Redis running » | Le service systemd `redis-server` est actif. Métrique `service_redis_server_active` : 1 actif, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « Redis answers » | Le port 6379 accepte une connexion TCP sur 127.0.0.1. Métrique `port_6379_up` | Critique en cas d'échec, pendant 1 minute |

L'unité s'appelle `redis-server` sous Debian et Ubuntu, `redis` sous RHEL et Rocky, où la métrique devient `service_redis_active`. Un service absent compte comme arrêté : un mauvais nom déclenche une alerte.

Une vérification de port prouve que Redis accepte les connexions TCP, pas qu'il répond aux commandes. Un court script va plus loin :

```sh
#!/bin/sh
if [ "$(redis-cli -h 127.0.0.1 ping 2>/dev/null)" = "PONG" ]; then
  echo "OK: Redis answers"; exit 0
fi
echo "CRITICAL: Redis does not answer PING"; exit 2
```

Si Redis exige un mot de passe, `ping` reçoit une erreur d'authentification et le script signale un problème. Voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md) pour l'installer.

La mémoire est vérifiée sur chaque serveur sans configuration : avertissement sous 10 % disponible, critique sous 5 %, pendant 5 minutes. Sur un serveur Redis, c'est souvent le premier signe d'ennui.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `Redis` dans « Find a check ».
3. Choisissez « Redis running ». Dans « Service », gardez `redis-server` ou saisissez `redis`.
4. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
5. Faites de même avec « Redis answers ». Si Redis n'écoute que sur une autre adresse (`bind` dans `redis.conf`), saisissez-la dans « Address ».

Les vérifications tournent toutes les 60 secondes par défaut. Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site et utilisez « Add a check », mais seulement si tous ses serveurs font tourner Redis : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la vérification et choisissez « Edit ». « Alert » règle critique ou avertissement (un cache qui ne fait que ralentir peut mériter un avertissement), « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite), « Apply to » ce serveur ou tous ceux du site. Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: service_redis_server_active redis-server.service is 0 on cache-1 (paris)
Critical: port_6379_up is 0 on cache-1 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand Redis revient.

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La cause de l'arrêt :** `systemctl status redis-server` et `journalctl -u redis-server -n 50`. L'onglet « Logs » du serveur affiche aussi les avertissements et erreurs du journal.
2. **La mémoire :** `journalctl -k | grep -i oom` montre si le noyau l'a tué. Vérifiez `maxmemory` dans `redis.conf`.
3. **L'espace disque :** `df -h`. Un instantané raté sur un disque plein peut amener Redis à refuser les écritures.
4. **Le service tourne mais le port échoue :** vérifiez `bind` et `port` dans `redis.conf`, et le pare-feu local.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
