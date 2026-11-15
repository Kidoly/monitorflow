---
title: "Comment surveiller SSH et fail2ban sous Linux"
description: "Soyez alerté quand le serveur SSH s'arrête, que le port 22 ne répond plus, que fail2ban cesse de bannir ou que les connexions ratées s'accumulent."
slug: surveiller-ssh-fail2ban
lang: fr
translation: monitor-ssh-fail2ban
---

# Comment surveiller SSH et fail2ban sous Linux

SSH, c'est par là que vous entrez sur un serveur Linux quand quelque chose ne va pas. Quand il s'arrête, vous le découvrez au pire moment, en pleine panne, avec une console pour seule issue. fail2ban, qui bannit les adresses qui devinent des mots de passe, tombe tout aussi discrètement. MonitorFlow surveille les deux, et compte les connexions ratées.

## Pourquoi c'est important

Un `sshd_config` erroné après une mise à jour, ou un paquet supprimé, et le serveur SSH s'arrête. Rien d'autre ne casse, donc personne ne le remarque avant d'avoir besoin de se connecter. fail2ban s'arrête quand un filtre ne se lit plus, et les attaques par force brute passent alors sans frein.

## Ce que MonitorFlow vérifie

Quatre entrées du catalogue de vérifications couvrent le sujet, toutes sous Linux :

| Entrée du catalogue | Ce qu'elle fait | Alerte à l'ajout |
| --- | --- | --- |
| « SSH server running » | L'unité systemd `ssh` (Debian, Ubuntu) ou `sshd` (RHEL, Rocky) est active. Métrique `service_ssh_active` ou `service_sshd_active` | Critique quand elle s'arrête, pendant 1 minute |
| « SSH answers » | Le port 22 accepte les connexions TCP. Métrique `port_22_up` | Critique quand il échoue, pendant 1 minute |
| « fail2ban running » | L'unité `fail2ban` est active. Métrique `service_fail2ban_active` | Critique quand elle s'arrête, pendant 1 minute |
| « Failed SSH logins » | Compte les lignes contenant `Failed password` dans `/var/log/auth.log`. Métrique `log_auth_matches` | Avertissement à chaque ligne trouvée, aussitôt |

Une unité absente compte comme arrêtée. Une vérification de port se connecte depuis l'agent, à `127.0.0.1` par défaut. Depuis Ubuntu 22.10, SSH démarre à la demande via `ssh.socket` et `ssh.service` peut rester inactif jusqu'à la première connexion : indiquez alors `ssh.socket` comme service.

La vérification de journal compte les lignes écrites après son démarrage, et il lui faut un fichier à lire. RHEL et Rocky écrivent ces lignes dans `/var/log/secure` ; un système sans rsyslog ne les garde que dans le journal systemd, sans `auth.log`. L'utilisateur de l'agent doit aussi pouvoir lire le fichier. Sous Debian et Ubuntu, `auth.log` appartient au groupe `adm`, qui lit tous les journaux de `/var/log` : `sudo usermod -aG adm monitorflow`, puis `sudo systemctl restart monitorflow-agent`. Sans accès, la vérification signale « access denied » et réessaie toutes les cinq minutes.

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans son onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `ssh` et prenez « SSH server running ». Vérifiez le champ « Service » : `ssh`, `sshd` ou `ssh.socket`.
4. Laissez « Raise critical when it fails » coché.
5. Sous « Add to », choisissez « This server only » ou « Every server of » suivi de son site, puis « Add check ».
6. Faites de même pour « SSH answers », « fail2ban running » et « Failed SSH logins », dont le « Log file » est `/var/log/secure` sous RHEL et Rocky.

Pour tout un site, passez par « Settings › Sites », « Alerts and checks » sous le site, puis « Add a check ».

## Modifier l'alerte

Dans l'onglet « Sensors », menu de la ligne, « Edit » : « Alert » choisit critique ou avertissement, « For at least » la durée en minutes, « Apply to » ce serveur ou tout le site. Sur la page « Alerts and checks » d'un site, c'est sous « When to alert », où la gravité s'appelle « Raise ».

Sur un serveur exposé à Internet, les connexions ratées ne cessent jamais. Réglez « For at least » sur 10 ou 15 minutes : l'alerte ne s'ouvre alors que si les tentatives continuent, à chaque vérification, pendant tout ce temps.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message donne la métrique, sa valeur, le serveur et son site :

```
Critical: service_ssh_active ssh.service is 0 on web-1 (paris)
Critical: port_22_up is 0 on web-1 (paris)
Warning: log_auth_matches is 14 on web-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. « Acknowledge » signale aux autres que vous vous en occupez. Vérifiez ensuite :

1. **Le service :** depuis une console, `systemctl status ssh` et `journalctl -u ssh -n 50`. Après une modification de configuration, `sshd -t` affiche l'erreur.
2. **Le port, si le service tourne :** `ss -ltnp | grep :22`.
3. **fail2ban :** `systemctl status fail2ban` et `fail2ban-client status sshd`.
4. **Les connexions ratées :** `grep "Failed password" /var/log/auth.log | tail` donne les adresses.

Pour compter d'autres lignes, voir [Comment surveiller les fichiers journaux](surveiller-fichiers-journaux.md).

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
