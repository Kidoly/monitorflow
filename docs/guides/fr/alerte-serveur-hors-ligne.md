---
title: "Comment être alerté quand un serveur tombe"
description: "Sachez en quelques minutes qu'un serveur Linux ou Windows ne répond plus, sans rien configurer, et soyez prévenu par e-mail ou messagerie."
slug: alerte-serveur-hors-ligne
lang: fr
translation: server-down-alerts
---

# Comment être alerté quand un serveur tombe

Un serveur qui ne répond plus, c'est l'alerte que tout le monde attend, et pourtant la plus facile à manquer : personne ne regarde une machine qui marche, et une panne la nuit se découvre le matin, par un utilisateur. MonitorFlow vous prévient en quelques minutes, sans aucune vérification à ajouter.

## Pourquoi c'est important

Un serveur tombe pour bien des raisons : un kernel panic, un disque plein qui arrête les services, un hôte privé de courant, un changement réseau, une VM suspendue par erreur. À chaque fois, ce qui tourne dessus s'arrête aussi. Sans alerte, vous l'apprenez par un ticket, une sauvegarde manquée ou un site qui ne s'affiche plus, souvent des heures plus tard.

## Ce que MonitorFlow vérifie

L'agent de chaque serveur garde une connexion ouverte vers le serveur central et se signale toutes les 30 secondes. Le serveur central surveille lui-même cette connexion :

- Un serveur sans contact depuis **plus de 5 minutes** ouvre une alerte **critique** : le serveur est hors ligne. L'alerte part du dernier contact, sa durée est donc celle de la vraie panne.
- Elle se résout d'elle-même quand l'agent se reconnecte.
- 5 minutes, c'est plus long qu'un redémarrage de l'agent, une mise à jour ou un redémarrage du serveur central : les opérations courantes ne déclenchent rien. Après un redémarrage du serveur central, les serveurs ont 5 minutes pour se reconnecter.
- Quand un site passe par un **relais** et que ce relais s'arrête, une seule alerte s'ouvre pour le relais, avec le nombre de serveurs qui ne peuvent plus se signaler derrière lui. Vous recevez un message, pas un par serveur.

C'est intégré, sous Linux comme sous Windows, et rien n'est à ouvrir sur le serveur : l'agent ne fait que des connexions sortantes.

## Mise en place

Il n'y a rien à ajouter. Pour vérifier que c'est actif sur un serveur :

1. Ouvrez « Fleet » et cliquez sur le serveur.
2. Dans les informations en haut de sa page, « Offline alerts » indique « On ».

Pour surveiller aussi un service de l'extérieur, et savoir quand il ne répond plus alors que l'agent tourne toujours, ajoutez une vérification de port sur un autre serveur. Ouvrez cet autre serveur, choisissez « Add a check », prenez « A port answers », puis saisissez le port et, dans « Address », le nom ou l'adresse IP du serveur à surveiller. Une alerte critique s'ouvre quand le port ne répond plus pendant une minute.

## Modifier l'alerte

Le délai de 5 minutes est fixe : un bon réglage par défaut est plus simple qu'une option. Ce que vous choisissez, c'est quelles machines sont surveillées :

- Sur la page du serveur, « Offline alerts » propose « Turn off », pour une machine que vous éteignez exprès, comme une VM de test. La désactivation ferme une alerte hors ligne déjà ouverte.
- Pour tout un site, ouvrez « Settings › Sites » : chaque site a son propre interrupteur.
- Pour une intervention prévue, utilisez plutôt un silence : sur la page du serveur, « Silence for » une durée, ou « Monitoring › Silences » pour un site ou pour tout. Pendant un silence, les alertes s'ouvrent et s'affichent toujours, mais personne n'est prévenu. Ce qui est encore ouvert à la fin est envoyé.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, pour un client ou pour un site. « Send test » envoie un message d'essai.

Plusieurs serveurs d'un site qui tombent ensemble sont envoyés dans un seul message. Une alerte critique que personne n'acquitte en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call », où vous pouvez aussi créer un planning d'astreinte et une relance.

## Quand l'alerte se déclenche

Le message ressemble à ceci, avec le serveur et son site :

```
Critical: web-1 (paris) is offline
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, et le corps contient un lien vers l'alerte. Quand le serveur revient, un second message indique `Resolved: web-1 (paris) is back online`.

Cliquez sur « Acknowledge » sur la page de l'alerte pour signaler aux autres que vous vous en occupez. Vérifiez ensuite, dans cet ordre :

1. **La machine est-elle allumée ?** Faites un ping, ou ouvrez sa console dans votre hyperviseur ou votre cloud.
2. **L'agent tourne-t-il ?** Sous Linux, `systemctl status monitorflow-agent` et `journalctl -u monitorflow-agent`. Sous Windows, `Get-Service MonitorFlowAgent`, et le journal de l'agent dans `C:\ProgramData\MonitorFlow\agent\logs\agent.log`.
3. **Joint-il le serveur central ?** L'agent a besoin des ports 7443 et 7445 du serveur central, ou de son relais. Un changement de pare-feu ou de DNS en est souvent la cause.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Collez ensuite sur chaque serveur la commande de « Settings › Add servers ». Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
