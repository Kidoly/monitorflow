---
title: "Comment surveiller Postfix sous Linux"
description: "Soyez alerté quand Postfix s'arrête, que le port 25 ne répond plus ou que la file d'attente grossit, pour que le courrier de vos serveurs continue de partir."
slug: surveiller-postfix
lang: fr
translation: monitor-postfix
---

# Comment surveiller Postfix sous Linux

Postfix achemine le courrier de vos serveurs et applications : réinitialisations de mot de passe, factures, rapports de cron. Quand il s'arrête ou ne peut plus livrer, les messages s'entassent sans un mot, et les utilisateurs s'en aperçoivent les premiers. MonitorFlow surveille le service, le port SMTP et, avec un court script, la file d'attente.

## Pourquoi c'est important

Postfix s'arrête après une mise à jour ou une erreur de configuration. Plus souvent, il tourne et ne livre rien : le relais refuse son mot de passe, un changement DNS casse la livraison, l'adresse du serveur atterrit sur une liste de blocage, ou le disque se remplit. Le courrier attend alors dans la file pendant des jours : une file qui grossit est le premier signe.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle fait | Alerte à l'ajout |
| --- | --- | --- |
| « Postfix running » | L'unité systemd `postfix` est active. Métrique `service_postfix_active` | Critique quand elle s'arrête, pendant 1 minute |
| « Mail server answers (SMTP) » | Le port 25 accepte les connexions TCP. Métrique `port_25_up` | Critique quand il échoue, pendant 1 minute |
| « Your own script » | Exécute un script que vous installez et lit son code de sortie : 0 OK, 1 avertissement, 2 critique, 3 inconnu. Métrique `script_<nom>_status` | Avertissement quand il signale un problème, pendant 1 minute |

Une unité absente compte comme arrêtée. La vérification de port se connecte depuis l'agent à `127.0.0.1` par défaut, ce qui convient à un Postfix qui écoute sur toutes les adresses ou seulement en local. Pour vérifier un autre serveur de messagerie, saisissez son nom dans « Address ».

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans son onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `postfix` et prenez « Postfix running ».
4. Laissez « Raise critical when it fails » coché, choisissez « This server only » ou « Every server of » suivi de son site sous « Add to », puis « Add check ».
5. Faites de même avec « Mail server answers (SMTP) ».

Pour tout un site, passez par « Settings › Sites », « Alerts and checks » sous le site, puis « Add a check ».

**La file d'attente.** Ce script compte les messages en attente, avertit au-delà de 50 et signale un état critique au-delà de 500. Sa ligne `queue=` est conservée comme métrique de la vérification. Il utilise `postqueue -j`, à partir de Postfix 3.1, que tout utilisateur peut lancer sauf si `authorized_mailq_users` en décide autrement :

```sh
#!/bin/sh
out=$(postqueue -j 2>/dev/null) || { echo "UNKNOWN: postqueue failed"; exit 3; }
count=$(printf '%s' "$out" | grep -c .)
echo "$count messages in the queue"
echo "queue=$count"
[ "$count" -gt 500 ] && exit 2
[ "$count" -gt 50 ] && exit 1
exit 0
```

Installez-le dans le dossier de scripts de l'agent, où seul root peut écrire, et essayez-le avec l'utilisateur de l'agent :

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_mailq /etc/monitorflow/scripts.d/
sudo -u monitorflow /etc/monitorflow/scripts.d/check_mailq; echo $?
```

Ajoutez ensuite « Your own script » avec `check_mailq` dans « Script », toutes les 5 minutes. Voir [Comment surveiller avec votre propre script](surveiller-avec-votre-script.md) pour les règles qu'un script suit.

## Modifier l'alerte

Dans l'onglet « Sensors », menu de la ligne, « Edit » : « Alert » choisit critique ou avertissement, « For at least » la durée en minutes, « Apply to » ce serveur ou tout le site. Sur la page « Alerts and checks » d'un site, c'est sous « When to alert », où la gravité s'appelle « Raise ».

Pour changer les limites de la file, modifiez les deux nombres du script. Un envoi groupé peut remplir la file une minute ; réglez « For at least » sur 15 minutes pour n'être prévenu que si elle ne se vide pas.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

Un canal e-mail qui passe par le Postfix surveillé ne peut pas vous signaler son arrêt : ajoutez aussi un canal de messagerie instantanée ou un webhook.

## Quand l'alerte se déclenche

Le message donne la métrique, sa valeur, le serveur et son site :

```
Critical: service_postfix_active postfix.service is 0 on mail-1 (paris)
Critical: port_25_up is 0 on mail-1 (paris)
Warning: script_check_mailq_status is 1 on mail-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. « Acknowledge » signale aux autres que vous vous en occupez. Vérifiez ensuite :

1. **Le service :** `systemctl status postfix` et `journalctl -u postfix -n 50`. `postfix check` signale les erreurs de configuration.
2. **La file :** `mailq` liste les messages en attente avec la raison de chacun, comme une authentification refusée ou un hôte introuvable.
3. **La livraison :** `grep "status=deferred" /var/log/mail.log | tail` sous Debian et Ubuntu, ou `/var/log/maillog` sous RHEL et Rocky. Une fois le problème réglé, `postqueue -f` relance la file.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
