---
title: "Comment tout surveiller avec votre propre script (Bash ou PowerShell)"
description: "Transformez un script shell ou PowerShell en vérification avec alerte : codes de sortie pour OK, avertissement et critique, lignes nom=valeur en métriques."
slug: surveiller-avec-votre-script
lang: fr
translation: monitor-with-your-own-script
---

# Comment tout surveiller avec votre propre script (Bash ou PowerShell)

Certaines choses, vous seul savez les vérifier : que la sauvegarde de cette nuit existe, qu'une API interne répond juste. MonitorFlow exécute un script installé sur le serveur et transforme son résultat en alerte ; les plugins de supervision au format habituel fonctionnent tels quels.

## Pourquoi c'est important

Les vérifications intégrées voient qu'un service tourne et qu'un port répond, pas qu'une sauvegarde a écrit un fichier vide ni qu'une file d'attente grossit depuis des heures. Un script de dix lignes qui teste le vrai résultat le voit.

## Ce que MonitorFlow vérifie

La vérification « A script reports OK », appelée « Your own script » dans le catalogue, exécute un script à chaque passage et lit :

- **Son code de sortie**, comme état (`script_…_status`) : `0` OK, `1` avertissement, `2` critique, `3` inconnu. Un script qui dépasse son délai, est tué, sort avec un autre code ou écrit plus de 64 Kio compte comme `3`.
- **Les lignes de la forme `nom=valeur`** sur sa sortie standard, chacune enregistrée comme métrique du serveur, par exemple `age_seconds=5400`. Les noms utilisent des minuscules, des chiffres et `_`, commencent par une lettre, et 32 au plus sont gardés. Les autres lignes, comme un résumé en première ligne, sont ignorées.
- **Sa durée**, comme `script_…_duration_seconds`.

La vérification porte le nom du fichier : `check_backup.sh` donne `script_check_backup_status`. Vous pouvez tracer les métriques `nom=valeur` sur un tableau de bord.

Par sécurité, le serveur central ne peut que nommer un script ; il ne peut jamais en envoyer un. L'agent n'exécute que les fichiers qu'un administrateur a installés dans son dossier de scripts :

- **Linux** : `/etc/monitorflow/scripts.d`. Le dossier et le fichier appartiennent à root (ou à l'utilisateur de l'agent) et ne sont pas modifiables par les autres. Le fichier est exécutable et commence par `#!`, ou c'est un programme compilé. Il tourne sous l'utilisateur de l'agent, `monitorflow`, avec un `PATH` minimal, depuis `/`, sans entrée.
- **Windows** : `C:\ProgramData\MonitorFlow\scripts.d`, un `.ps1` exécuté par Windows PowerShell 5.1 ou un `.exe`. Les fichiers batch sont refusés. Il tourne sous le compte de service de l'agent.

Le serveur central n'envoie aucun argument sauf autorisation de l'administrateur du serveur : écrivez les valeurs utiles dans le script.

## Mise en place

Écrivez le script. Celui-ci vérifie qu'une sauvegarde a moins de 26 heures :

```sh
#!/bin/sh
newest=$(find /srv/backup -type f -printf '%T@\n' 2>/dev/null | sort -n | tail -1)
[ -z "$newest" ] && { echo "No backup found"; exit 2; }
age=$(( $(date +%s) - ${newest%.*} ))
echo "Newest backup is $((age / 3600)) h old"
echo "age_seconds=$age"
[ "$age" -lt 93600 ] && exit 0
[ "$age" -lt 172800 ] && exit 1
exit 2
```

Installez-le en root, vérifiez que l'utilisateur `monitorflow` peut lire ce qu'il teste, et essayez-le avec les contrôles de l'agent :

```sh
install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
install -o root -g root -m 0755 check_backup.sh /etc/monitorflow/scripts.d/
monitorflow-agent sensors probe script --param script=check_backup.sh
```

Sous Windows, copiez un `.ps1` dans `C:\ProgramData\MonitorFlow\scripts.d` depuis un PowerShell administrateur, terminez-le par `exit 0`, `1`, `2` ou `3`, et interceptez vous-même les erreurs : une erreur non interceptée sort avec 1, un avertissement.

Ensuite, dans l'interface web :

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur son onglet « Sensors ».
2. Choisissez « Add a check », tapez `script` dans « Find a check » et prenez « Your own script ».
3. Dans « Script », saisissez seulement le nom du fichier : `check_backup.sh`. Pour une sauvegarde quotidienne, « How often » une fois par heure suffit.
4. Laissez « Raise warning when it fails » coché, choisissez « This server only » ou « Every server of » suivi du site, puis « Add check ».

Un plugin empaqueté fonctionne aussi. « Pending updates (check_apt) » demande d'abord de lier `check_apt` dans le dossier : `ln -s /usr/lib/nagios/plugins/check_apt /etc/monitorflow/scripts.d/`. Liez les plugins un par un, jamais tout le dossier.

Si la ligne reste sur « Waiting for first value », l'agent a refusé le script : absent, modifiable par d'autres, ou sans ligne `#!`. `sensors probe` donne la raison. Une fois corrigé, l'agent réessaie dans les 5 minutes.

## Modifier l'alerte

Par défaut, tout état autre que OK pendant une minute lève un **avertissement**. Dans l'onglet « Sensors », ouvrez le menu de la ligne de la vérification et choisissez « Edit » : « Alert » la passe en critique, « For at least » fixe combien de temps l'état doit durer, « Timeout » laisse plus de temps à un script lent, et « Apply to » choisit ce serveur ou tout le site.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message donne le code d'état et le serveur :

```
Warning: script_check_backup_status is 2 on files-1 (paris)
```

Cliquez sur « Acknowledge » sur la page de l'alerte, puis lancez le script avec `monitorflow-agent sensors probe`, comme plus haut, pour voir sa sortie. Un état `3` signifie souvent que le script lui-même a échoué : une commande absente du `PATH` minimal, un fichier illisible pour l'utilisateur de l'agent, ou un délai dépassé.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
