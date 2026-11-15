---
title: "Comment surveiller PostgreSQL"
description: "Soyez alerté quand PostgreSQL s'arrête ou n'accepte plus de connexions sur un serveur Linux, avec deux vérifications du catalogue MonitorFlow."
slug: surveiller-postgresql
lang: fr
translation: monitor-postgresql
---

# Comment surveiller PostgreSQL

Quand PostgreSQL s'arrête, les applications qui en dépendent s'arrêtent aussi, et le premier signe est souvent une page d'erreur vue par un autre. MonitorFlow surveille le service PostgreSQL et son port, et vous prévient en une à deux minutes.

## Pourquoi c'est important

PostgreSQL plante rarement seul. Il s'arrête à cause de son environnement : disque rempli de fichiers WAL, noyau qui le tue faute de mémoire, mise à jour qui l'a relancé avec une configuration invalide, cluster non démarré après un redémarrage. L'application consigne des erreurs de connexion que personne ne lit, et vous l'apprenez par un utilisateur.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle vérifie | Alerte par défaut |
| --- | --- | --- |
| « PostgreSQL running » | Le service systemd `postgresql` est actif. Métrique `service_postgresql_active` : 1 actif, 0 sinon | Critique s'il s'arrête, pendant 1 minute |
| « PostgreSQL answers » | Le port 5432 accepte une connexion TCP sur 127.0.0.1. Métrique `port_5432_up` | Critique en cas d'échec, pendant 1 minute |

Un service absent compte comme arrêté : une faute de frappe dans le nom de l'unité déclenche une alerte au lieu de passer inaperçue.

**Sous Debian et Ubuntu, vérifiez l'unité du cluster.** `postgresql.service` y est une unité parapluie qui reste active même base arrêtée. Le serveur tourne sous `postgresql@<version>-<cluster>`, par exemple `postgresql@16-main` (`systemctl list-units 'postgresql@*'` affiche le vôtre) : saisissez ce nom. Sous RHEL et Rocky, `postgresql` convient (`postgresql-16` avec les paquets de la communauté).

Une vérification de port prouve que PostgreSQL accepte les connexions TCP, pas qu'il répond aux requêtes. Pour aller plus loin, appelez `pg_isready` depuis un script : ses codes de sortie (0 accepte, 1 refuse, 2 pas de réponse) sont ceux que MonitorFlow lit comme OK, avertissement et critique. Voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md).

L'espace disque est vérifié sur chaque serveur sans configuration : avertissement à 85 % de remplissage, critique à 95 %.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `PostgreSQL` dans « Find a check ».
3. Choisissez « PostgreSQL running ». Dans « Service », gardez `postgresql` ou saisissez l'unité de votre cluster.
4. Laissez « Raise critical when it fails » coché. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis cliquez sur « Add check ».
5. Faites de même avec « PostgreSQL answers ». Si PostgreSQL n'écoute que sur une autre adresse, saisissez-la dans « Address ».

Les vérifications tournent toutes les 60 secondes par défaut. Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site et utilisez « Add a check », mais seulement si tous ses serveurs font tourner PostgreSQL : sur les autres, le service absent déclenche une alerte.

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la vérification et choisissez « Edit ». « Alert » règle critique ou avertissement, « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite), « Apply to » ce serveur ou tous ceux du site. Sur la page « Alerts and checks » d'un site, les mêmes réglages sont sous « When to alert », avec « Raise » pour la gravité.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: service_postgresql_active postgresql.service is 0 on db-1 (paris)
Critical: port_5432_up is 0 on db-1 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand PostgreSQL revient.

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La cause de l'arrêt :** `systemctl status` et `journalctl -u` avec votre unité, par exemple `postgresql@16-main`. L'onglet « Logs » du serveur affiche aussi les avertissements et erreurs du journal.
2. **L'espace disque :** un volume de données ou de WAL plein arrête PostgreSQL. Lancez `df -h`.
3. **La mémoire :** `journalctl -k | grep -i oom` montre si le noyau l'a tué.
4. **Le service tourne mais le port échoue :** vérifiez `listen_addresses` et `port` dans `postgresql.conf`, et le pare-feu local.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
