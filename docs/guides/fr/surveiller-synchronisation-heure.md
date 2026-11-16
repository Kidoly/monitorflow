---
title: "Comment surveiller la synchronisation de l'heure (NTP) sous Linux et Windows"
description: "Soyez alerté quand chrony, systemd-timesyncd ou le service de temps Windows s'arrête, et quand l'horloge dérive, avant l'échec des connexions."
slug: surveiller-synchronisation-heure
lang: fr
translation: monitor-time-sync
---

# Comment surveiller la synchronisation de l'heure (NTP) sous Linux et Windows

L'horloge d'un serveur dérive de quelques secondes par jour dès que rien ne la corrige. Pendant un temps, rien ne se voit. Puis les connexions Kerberos échouent, les certificats TLS semblent pas encore valides, les réplicas d'une base ne sont plus d'accord, et les journaux de deux machines ne concordent plus au moment où vous en avez le plus besoin. MonitorFlow vous prévient quand le service de temps s'arrête, et un court script peut surveiller la dérive elle-même.

## Pourquoi c'est important

Le service de temps s'arrête après une mise à jour, une erreur de configuration ou un conflit entre deux services qui veulent tous deux régler l'horloge. Quand quelqu'un s'en aperçoit, l'horloge a des minutes d'écart, et les symptômes pointent partout sauf vers elle : par défaut, un domaine Active Directory refuse les connexions au-delà de cinq minutes d'écart.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Système | Ce qu'elle fait | Alerte à l'ajout |
| --- | --- | --- | --- |
| « Time sync running » | Linux | L'unité systemd `chrony` est active. Indiquez `chronyd` sous RHEL et Rocky, ou `systemd-timesyncd` si c'est le service de temps. Métrique `service_chrony_active` | Critique quand elle s'arrête, pendant 1 minute |
| « Windows Time running » | Windows | Le service Windows `W32Time` tourne. Métrique `service_w32time_active` | Critique quand il s'arrête, pendant 1 minute |

Une unité ou un service absent compte comme arrêté : une vérification avec un mauvais nom alerte aussitôt et vous montre l'erreur. MonitorFlow ne mesure pas lui-même l'écart de l'horloge. Un script le peut, comme ci-dessous.

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans son onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `time` et prenez « Time sync running » sous Linux ou « Windows Time running » sous Windows.
4. Vérifiez le champ « Service » : sous Linux, `systemctl list-units --type=service | grep -E 'chrony|timesyncd'` affiche celui de votre serveur.
5. Laissez « Raise critical when it fails » coché, choisissez « This server only » ou « Every server of » suivi de son site sous « Add to », puis « Add check ».

Pour tout un site, passez par « Settings › Sites », « Alerts and checks » sous le site, puis « Add a check ».

**Surveiller l'écart avec chrony.** Ce script signale un avertissement quand l'horloge a plus de 0,1 s d'écart, un état critique au-delà de 1 s, et un état critique quand chrony n'est pas synchronisé. Sa ligne `offset_seconds=` est conservée comme métrique de la vérification :

```sh
#!/bin/sh
out=$(chronyc tracking 2>/dev/null) || { echo "UNKNOWN: chronyc failed"; exit 3; }
if printf '%s\n' "$out" | grep -q '^Leap status *: Not synchronised'; then
  echo "CRITICAL: not synchronised"; exit 2
fi
offset=$(printf '%s\n' "$out" | awk '/^System time/ {print $4}')
echo "Clock offset $offset s"
echo "offset_seconds=$offset"
awk -v o="$offset" 'BEGIN { exit (o > 1) ? 2 : (o > 0.1) ? 1 : 0 }'
```

Installez-le dans le dossier de scripts de l'agent et essayez-le avec l'utilisateur de l'agent :

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_clock /etc/monitorflow/scripts.d/
sudo -u monitorflow /etc/monitorflow/scripts.d/check_clock; echo $?
```

Ajoutez ensuite « Your own script » avec `check_clock` dans « Script ». Voir [Comment surveiller avec votre propre script](surveiller-avec-votre-script.md) pour les règles qu'un script suit, sous Linux comme sous Windows.

## Modifier l'alerte

Dans l'onglet « Sensors », menu de la ligne, « Edit » : « Alert » choisit critique ou avertissement, « For at least » la durée en minutes, « Apply to » ce serveur ou tout le site. Sur la page « Alerts and checks » d'un site, c'est sous « When to alert », où la gravité s'appelle « Raise ».

Pour changer les limites d'écart, modifiez les deux nombres de la dernière ligne du script. L'alerte d'un script est un avertissement par défaut : choisissez « Critical when it fails » pour les contrôleurs de domaine et les serveurs de bases de données.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message donne la métrique, sa valeur, le serveur et son site :

```
Critical: service_chrony_active chrony.service is 0 on db-1 (paris)
Critical: service_w32time_active W32Time is 0 on dc-1 (paris)
Warning: script_check_clock_status is 1 on db-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. « Acknowledge » signale aux autres que vous vous en occupez. Vérifiez ensuite :

1. **Le service :** `systemctl status chrony` et `journalctl -u chrony -n 50`, ou `Get-Service W32Time` sous Windows. Deux services de temps qui se disputent l'horloge sont une cause fréquente.
2. **Ses sources :** `chronyc sources` montre si un serveur de temps répond ; `w32tm /query /status` sous Windows. Un pare-feu qui bloque le port UDP 123 laisse le service tourner sans rien à suivre.
3. **L'écart :** `chronyc tracking`, ou `w32tm /stripchart /computer:<serveur de temps> /samples:3` sous Windows.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
