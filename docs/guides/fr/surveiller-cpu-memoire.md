---
title: "Comment surveiller le processeur et la mémoire de serveurs Linux et Windows"
description: "Soyez alerté quand le processeur d'un serveur reste saturé ou que sa mémoire s'épuise, avec des seuils intégrés modifiables par serveur ou par site."
slug: surveiller-cpu-memoire
lang: fr
translation: monitor-cpu-memory
---

# Comment surveiller le processeur et la mémoire de serveurs Linux et Windows

Un serveur qui manque de processeur ou de mémoire s'arrête rarement d'un coup. Il ralentit, puis le noyau tue des processus, et le premier signe est un utilisateur qui trouve l'application lente. MonitorFlow mesure le processeur et la mémoire de chaque serveur dès que son agent se connecte.

## Pourquoi c'est important

Un processus en boucle, une fuite mémoire qui grossit pendant des jours, une sauvegarde aux heures chargées, une VM trop juste : tout finit pareil. Les temps de réponse grimpent, le swap se remplit et, sous Linux, l'OOM killer arrête un processus, parfois votre base de données. Un pic de quelques secondes est normal ; ce qu'il faut savoir, c'est quand il dure.

## Ce que MonitorFlow vérifie

Deux vérifications tournent sur chaque serveur, Linux comme Windows, sans rien ajouter. Le catalogue les indique comme déjà actives :

| Vérification | Ce qu'elle mesure | Alerte par défaut |
| --- | --- | --- |
| « CPU usage » | Part du processeur utilisée, au total et par cœur (`cpu_usage_ratio`), et sous Linux la charge moyenne | Avertissement au-dessus de 90 % pendant 10 minutes. Pas de niveau critique |
| « Memory » | Mémoire disponible (`memory_available_ratio`) et swap | Avertissement sous 10 % pendant 5 minutes, critique sous 5 % |

La mémoire disponible est ce que les programmes peuvent obtenir sans swapper, comme la colonne `available` de `free` : le cache de pages ne compte pas.

Ces vérifications tournent toutes les minutes. La ligne « Built-in checks run every 1 min on every server of », sous la liste de l'onglet « Sensors », change cette fréquence pour tout le site.

Le catalogue propose aussi « Out of memory kills », non ajoutée d'office. Elle compte les lignes `Out of memory|oom-kill` de `/var/log/kern.log` : vous savez quand le noyau a tué un processus, même si la mémoire semble normale une minute après.

## Mise en place

Le processeur et la mémoire n'ont besoin de rien. Pour ajouter la vérification des OOM kills sur un serveur Linux :

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur son onglet « Sensors ».
2. Choisissez « Add a check », tapez `memory` dans « Find a check » et prenez « Out of memory kills ». Le formulaire est prérempli.
3. Sous « Add to », choisissez « This server only » ou « Every server of » suivi du site, puis « Add check ».

Elle lève un avertissement dès la première ligne écrite après son ajout. `/var/log/kern.log` existe là où rsyslog l'écrit, comme sous Debian et Ubuntu, et l'utilisateur de l'agent doit faire partie du groupe `adm` pour le lire : voir [Comment surveiller les fichiers journaux](surveiller-fichiers-journaux.md). Les erreurs du noyau apparaissent aussi dans l'onglet « Logs » du serveur, qui reçoit les avertissements et erreurs du journal systemd.

## Modifier l'alerte

Dans l'onglet « Sensors » du serveur, ouvrez le menu de la ligne « CPU usage » ou « Memory » et choisissez « Edit ». Réglez « Warning at » et « Critical at » en pourcentage (un champ vide supprime ce niveau), et « For at least » en minutes. Sous « Apply to », choisissez ce serveur seulement ou tous les serveurs du site. Pour un serveur de compilation qui tourne à plein exprès, montez l'avertissement à 98 % pendant 30 minutes, pour lui seul.

Pour tout un site, ouvrez « Settings › Sites », puis « Alerts and checks », et utilisez « Edit » sous « When to alert ». La valeur propre à un serveur l'emporte toujours sur celle du site.

Une alerte se résout quand la valeur revient 2 points au-delà du seuil : une valeur posée sur la limite ne l'ouvre et ne la ferme pas en boucle.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents », sur la vue d'ensemble et sur la page du serveur. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches, ou aux personnes désignées sur la page « On call ».

## Quand l'alerte se déclenche

Le message donne la métrique, sa valeur en fraction de 1, le serveur et son site :

```
Warning: cpu_usage_ratio total is 0.96 on app-2 (paris)
Critical: memory_available_ratio is 0.04 on db-1 (paris)
```

Un avertissement qui s'aggrave envoie `Escalated to critical: …`, puis `Resolved: …` à la fin. Cliquez sur « Acknowledge » sur la page de l'alerte pour signaler que vous vous en occupez, puis :

1. **Trouvez le processus.** `top` trié par processeur ou mémoire sous Linux, le Gestionnaire des tâches ou `Get-Process | Sort-Object CPU -Descending` sous Windows.
2. **Regardez la tendance.** « Open chart » dans le menu de la ligne : une montée lente sur des jours évoque une fuite, une marche une nouvelle tâche ou un déploiement.
3. **Consultez le noyau.** Sous Linux, `dmesg` ou l'onglet « Logs » pour les OOM kills, et `free -h` pour le swap.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
