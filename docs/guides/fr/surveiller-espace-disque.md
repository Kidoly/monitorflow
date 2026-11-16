---
title: "Comment surveiller l'espace disque de serveurs Linux et Windows"
description: "Soyez prévenu avant qu'un disque se remplisse et arrête vos services : alertes par disque à 85 % et 95 % d'office, et alertes de disque lent."
slug: surveiller-espace-disque
lang: fr
translation: monitor-disk-space
---

# Comment surveiller l'espace disque de serveurs Linux et Windows

Un disque plein est la panne la plus courante que personne n'a vue venir, alors qu'elle s'annonçait depuis des semaines. MonitorFlow surveille chaque disque de chaque serveur et vous prévient tant qu'il reste de la marge.

## Pourquoi c'est important

Quand un disque se remplit, les bases de données n'écrivent plus, des journaux se perdent, les mises à jour échouent à mi-chemin et des services refusent de démarrer. La cause est souvent lente : journaux jamais tournés, vieilles sauvegardes, images de conteneurs. Rien ne casse avant le tout dernier bloc.

Un disque peut aussi être lent plutôt que plein : un disque qui faiblit, une baie de stockage surchargée. Tout ce qui tourne dessus attend.

## Ce que MonitorFlow vérifie

Deux vérifications tournent sur chaque serveur, sans rien ajouter. Le catalogue des vérifications les indique comme déjà actives :

| Vérification | Ce qu'elle mesure | Alerte par défaut |
| --- | --- | --- |
| « Disk space » | Espace utilisé sur chaque disque (`disk_used_ratio`, comme `df`), et les inodes quand le système de fichiers en limite le nombre | Avertissement au-delà de 85 % de remplissage pendant 5 minutes, critique au-delà de 95 % |
| « Disk activity » | L'occupation de chaque disque (« Disk busy ») et le temps qu'il met à répondre à une lecture ou une écriture (« Disk wait », `diskio_wait_seconds`) | Avertissement quand l'attente moyenne d'un disque dépasse 200 ms pendant 10 minutes |

Sous Linux, chaque système de fichiers monté compte comme un disque, partages réseau (NFS, SMB) compris ; `tmpfs`, `proc`, `overlay` et autres pseudo-systèmes sont ignorés. Un partage qui ne répond plus est sauté pour ce tour. Sous Windows, chaque lecteur fixe doté d'une lettre est surveillé (`C:\`, `D:\`).

## Mise en place

Il n'y a rien à ajouter : les alertes s'appliquent à chaque disque de chaque serveur dès sa connexion. Ouvrez « Fleet », cliquez sur le serveur : son onglet « Sensors » a une ligne par disque, et la colonne « Rule » indique quand chacun déclenche une alerte.

## Modifier l'alerte

Les valeurs par défaut conviennent rarement à tous les disques : un volume de 2 To à 85 % garde 300 Go libres, un `/boot` de 1 Go à 85 % est presque plein.

1. Dans l'onglet « Sensors » du serveur, ouvrez le menu de la ligne du disque et choisissez « Edit ».
2. Réglez « Warning at » et « Critical at » en pourcentage, et « For at least » en minutes.
3. Sous « Apply to », choisissez ce disque seulement, tous les disques du serveur, ou tous les serveurs du site (la valeur par défaut du site). Une valeur propre à un disque l'emporte sur celle de tous les disques.
4. « Save ».

Pour ne plus être alerté sur un disque, comme un volume de travail fait pour se remplir, choisissez « Turn off… » dans le menu de sa ligne. Pour tout un site, ouvrez « Settings › Sites », puis « Alerts and checks » pour le site, et utilisez « Edit » sous « When to alert ».

L'alerte de disque lent se règle de la même façon sur les lignes « Disk activity ». Un disque occupé qui répond vite n'est pas un problème : il n'y a donc pas d'alerte sur l'occupation.

Une alerte se résout quand la valeur repasse 2 points sous son seuil, pour éviter qu'elle s'ouvre et se ferme en boucle.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message donne le disque, son remplissage en fraction de 1, le serveur et son site :

```
Warning: disk_used_ratio /var is 0.87 on web-1 (paris)
Escalated to critical: disk_used_ratio /var is 0.96 on web-1 (paris)
```

Cliquez sur « Acknowledge » sur la page de l'alerte, puis cherchez ce qui a grossi :

- **Linux** : `df -h` et `df -i` (un disque peut manquer d'inodes avec de l'espace libre), puis `sudo du -xh --max-depth=1 /var | sort -h` pour trouver les plus gros dossiers. Les suspects habituels sont `/var/log`, le journal systemd (`journalctl --disk-usage`), les anciens noyaux, les caches de paquets et les images de conteneurs.
- **Windows** : les paramètres de stockage, ou `Get-ChildItem -Recurse | Sort-Object Length -Descending | Select-Object -First 20` dans le dossier suspect, et les journaux d'IIS ou de SQL Server s'ils tournent sur la machine.
- **Un disque lent** : `iostat -x 5` sous Linux montre quel périphérique attend, et l'hyperviseur ou la baie de stockage dit généralement pourquoi.

Le graphique de la ligne (« Open chart » dans son menu) montre à quelle vitesse le disque s'est rempli, ce qui distingue un journal qui s'emballe d'une croissance lente.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
