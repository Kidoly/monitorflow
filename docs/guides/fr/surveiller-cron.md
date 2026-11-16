---
title: "Comment surveiller cron et les tâches planifiées sous Linux"
description: "Soyez alerté quand cron s'arrête sur un serveur Linux, et quand une tâche nocturne, comme une sauvegarde, n'a pas produit son résultat à temps."
slug: surveiller-cron
lang: fr
translation: monitor-cron
---

# Comment surveiller cron et les tâches planifiées sous Linux

Sauvegardes, renouvellements de certificats, purges de journaux, rapports : tout passe par cron. Quand cron s'arrête, ou qu'une tâche échoue en silence chaque nuit, rien ne vous prévient : aucune erreur à l'écran, seulement un résultat absent le jour où vous en avez besoin. MonitorFlow surveille le service cron et, avec un court script, le résultat des tâches qui comptent.

## Pourquoi c'est important

cron peut s'arrêter après une mise à jour, un paquet supprimé ou une erreur pendant une maintenance. Le plus souvent, cron tourne et c'est une tâche qui échoue : un disque plein, un mot de passe expiré, un chemin déplacé, un script qui s'arrête trop tôt. Vous l'apprenez le jour où il vous faut la sauvegarde de la veille et que la plus récente date de trois semaines.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle fait | Alerte à l'ajout |
| --- | --- | --- |
| « Cron running » | L'unité systemd `cron` (Debian, Ubuntu) ou `crond` (RHEL, Rocky) est active. Métrique `service_cron_active` ou `service_crond_active` | Critique quand elle s'arrête, pendant 1 minute |
| « Your own script » | Exécute un script installé sur le serveur et lit son code de sortie : 0 OK, 1 avertissement, 2 critique, 3 inconnu. Métrique `script_<nom>_status` | Avertissement quand il signale un problème, pendant 1 minute |

Une unité absente compte comme arrêtée. La vérification du service vous dit que cron tourne ; elle ne peut pas dire si une tâche a fait son travail. Pour cela, vérifiez le résultat de la tâche : l'âge du fichier qu'elle écrit.

## Mise en place

**Le service cron :**

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans son onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `cron` et prenez « Cron running ». Indiquez `cron` ou `crond` dans « Service ».
4. Laissez « Raise critical when it fails » coché, choisissez « This server only » ou « Every server of » suivi de son site sous « Add to », puis « Add check ».

Pour tout un site, passez par « Settings › Sites », « Alerts and checks » sous le site, puis « Add a check ».

**Le résultat d'une tâche.** Ce script signale un état critique quand aucun fichier du dossier de sauvegarde n'a moins de 26 heures, ce qui laisse deux heures de marge à une tâche nocturne :

```sh
#!/bin/sh
dir=/srv/backup
if [ -n "$(find "$dir" -type f -mmin -1560 2>/dev/null | head -n 1)" ]; then
  echo "OK: a backup is less than 26 hours old"
  exit 0
fi
echo "CRITICAL: no backup in $dir for 26 hours"
exit 2
```

Installez-le en tant que root dans le dossier de scripts de l'agent, où seul root peut écrire :

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_backup_age /etc/monitorflow/scripts.d/
sudo -u monitorflow /etc/monitorflow/scripts.d/check_backup_age; echo $?
```

La dernière ligne l'exécute avec l'utilisateur de l'agent, qui doit pouvoir lister le dossier. Ensuite, dans l'onglet « Sensors », « Add a check », prenez « Your own script », saisissez `check_backup_age` dans « Script » et choisissez « How often » : toutes les 15 minutes ou toutes les heures suffit. [Comment surveiller avec votre propre script](surveiller-avec-votre-script.md) détaille les règles qu'un script doit suivre.

## Modifier l'alerte

Dans l'onglet « Sensors », menu de la ligne, « Edit » : « Alert » choisit critique ou avertissement, « For at least » la durée en minutes, « Apply to » ce serveur ou tout le site. Sur la page « Alerts and checks » d'un site, c'est sous « When to alert », où la gravité s'appelle « Raise ».

L'alerte d'un script est un avertissement par défaut ; une sauvegarde manquante mérite « Critical when it fails ». Avec une vérification toutes les heures, l'alerte s'ouvre au deuxième échec. Réglez « For at least » sur 0 pour être alerté dès le premier.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message donne la métrique, sa valeur, le serveur et son site :

```
Critical: service_cron_active cron.service is 0 on db-1 (paris)
Critical: script_check_backup_age_status is 2 on db-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. « Acknowledge » signale aux autres que vous vous en occupez. Vérifiez ensuite :

1. **cron :** `systemctl status cron` (ou `crond`) et `journalctl -u cron -n 50`.
2. **La tâche :** `grep CRON /var/log/syslog | tail` sous Debian et Ubuntu, ou `journalctl -u cron`, montre si elle a démarré. Lancez sa commande à la main avec son utilisateur pour voir l'erreur.
3. **Sa cible :** un disque plein, un partage non monté ou un mot de passe changé sont les causes habituelles. Voir [Comment surveiller l'espace disque](surveiller-espace-disque.md).

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
