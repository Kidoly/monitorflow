---
title: "Comment surveiller nginx"
description: "Soyez alerté quand nginx s'arrête, quand les ports 80 et 443 ne répondent plus ou quand son journal signale des erreurs critiques."
slug: surveiller-nginx
lang: fr
translation: monitor-nginx
---

# Comment surveiller nginx

nginx se trouve devant l'essentiel de ce qu'offre un serveur Linux : sites, API, proxys inverses. Quand il s'arrête, tout ce qui est derrière disparaît. MonitorFlow surveille le service nginx, ses ports et son journal d'erreurs.

## Pourquoi c'est important

nginx plante rarement seul. Il s'arrête à cause de son environnement : une faute dans la configuration qui l'empêche de redémarrer, un certificat déplacé, un disque plein, une mise à jour. Vu de l'extérieur, c'est pareil : le site est inaccessible, et un visiteur le remarque en premier. Le journal d'erreurs l'annonçait souvent : lignes `[crit]` sur des fichiers illisibles, `[emerg]` quand une configuration ne se charge pas.

## Ce que MonitorFlow vérifie

Cherchez « nginx » dans le catalogue des vérifications :

| Entrée du catalogue | Ce qu'elle fait | Alerte par défaut |
| --- | --- | --- |
| « nginx running » | Le service systemd `nginx` est actif. Métrique `service_nginx_active`, 1 s'il est actif | Critique s'il s'arrête, pendant 1 minute |
| « Web server answers (HTTP) » | Le port 80 accepte les connexions TCP. Métrique `port_80_up` | Critique en cas d'échec, pendant 1 minute |
| « Web server answers (HTTPS) » | Le port 443 accepte les connexions TCP. Métrique `port_443_up` | Critique en cas d'échec, pendant 1 minute |
| « nginx critical errors » | Compte les lignes de `/var/log/nginx/error.log` marquées `[crit]`, `[alert]` ou `[emerg]`. Métrique `log_error_matches` | Avertissement à chaque ligne trouvée, aussitôt |

Un service absent compte comme arrêté. Une vérification de port ouvre seulement une connexion TCP : elle prouve que nginx accepte les connexions, pas que les pages s'affichent ; pour cela, voir [Comment surveiller un site web en HTTPS](surveiller-site-web-https.md). La vérification du journal ne compte que les nouvelles lignes et suit la rotation du fichier.

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans l'onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `nginx` et choisissez « nginx running ». Le champ « Service » contient `nginx`.
4. Laissez « Raise critical when it fails » coché.
5. Sous « Add to », choisissez « This server only » ou « Every server of » son site.
6. Cliquez sur « Add check ».

Procédez de même pour les deux ports et « nginx critical errors ». Une vérification de port se connecte à `127.0.0.1` ; si nginx écoute ailleurs, indiquez l'adresse dans « Address ». Pour tout un site, ouvrez « Settings › Sites », cliquez sur « Alerts and checks » sous le site, puis sur « Add a check ».

**Si la vérification du journal affiche « access denied ».** Sous Debian et Ubuntu, les journaux de nginx appartiennent au groupe `adm`, dont l'utilisateur de l'agent, `monitorflow`, ne fait pas partie. Lancez `sudo usermod -aG adm monitorflow`, puis `sudo systemctl restart monitorflow-agent`. Le groupe `adm` lit aussi d'autres journaux de `/var/log`. L'agent retente la vérification toutes les 5 minutes.

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». « Alert » choisit critique ou avertissement, « For at least » le nombre de minutes que doit durer le problème (`0` alerte dès la première mauvaise valeur), et « Apply to » ce serveur seul ou tous ceux du site. Pour un site, utilisez « When to alert » sur sa page « Alerts and checks ».

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Send test » le vérifie. Si nginx tombe sur plusieurs serveurs à la fois, un seul message part. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: service_nginx_active nginx.service is 0 on web-1 (paris)
Critical: port_443_up is 0 on web-1 (paris)
Warning: log_error_matches is 3 on web-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Un message `Resolved:` suit la résolution. Cliquez sur « Acknowledge », puis vérifiez :

1. **Pourquoi il s'est arrêté :** `systemctl status nginx` et `journalctl -u nginx -n 50`, ou l'onglet « Logs » du serveur.
2. **La configuration :** `sudo nginx -t` indique le fichier et la ligne fautifs.
3. **Le journal d'erreurs :** `tail -n 50 /var/log/nginx/error.log`.
4. **Disque et mémoire :** tous deux peuvent arrêter nginx ([espace disque](surveiller-espace-disque.md), [CPU et mémoire](surveiller-cpu-memoire.md)).

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
