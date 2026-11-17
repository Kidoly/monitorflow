---
title: "Surveiller vos serveurs avec MonitorFlow"
description: "Des guides pas à pas pour surveiller bases de données, serveurs web, services Linux et Windows, disques et journaux avec MonitorFlow."
slug: guides
lang: fr
translation: guides
---

# Surveiller vos serveurs avec MonitorFlow

Chaque guide explique ce qui peut mal tourner, ce que MonitorFlow vérifie, comment ajouter la vérification, à quoi ressemble l'alerte et quoi regarder en premier quand elle se déclenche. Tous partent d'un serveur où l'agent MonitorFlow est installé.

Les guides existent aussi [en anglais](../README.md).

## Serveurs

- [Comment être alerté quand un serveur tombe](alerte-serveur-hors-ligne.md) : Sachez en quelques minutes qu'un serveur Linux ou Windows ne répond plus, sans rien configurer, et soyez prévenu par e-mail ou messagerie.
- [Comment surveiller le processeur et la mémoire de serveurs Linux et Windows](surveiller-cpu-memoire.md) : Soyez alerté quand le processeur d'un serveur reste saturé ou que sa mémoire s'épuise, avec des seuils intégrés modifiables par serveur ou par site.
- [Comment surveiller l'espace disque de serveurs Linux et Windows](surveiller-espace-disque.md) : Soyez prévenu avant qu'un disque se remplisse et arrête vos services : alertes par disque à 85 % et 95 % d'office, et alertes de disque lent.
- [Comment surveiller les erreurs des fichiers journaux sous Linux et Windows](surveiller-fichiers-journaux.md) : Soyez alerté quand une erreur, un échec de connexion ou un motif choisi apparaît dans un fichier journal, et lisez les journaux système au même endroit.
- [Comment tout surveiller avec votre propre script (Bash ou PowerShell)](surveiller-avec-votre-script.md) : Transformez un script shell ou PowerShell en vérification avec alerte : codes de sortie pour OK, avertissement et critique, lignes nom=valeur en métriques.

## Bases de données

- [Comment surveiller PostgreSQL](surveiller-postgresql.md) : Soyez alerté quand PostgreSQL s'arrête ou n'accepte plus de connexions sur un serveur Linux, avec deux vérifications du catalogue MonitorFlow.
- [Comment surveiller MySQL et MariaDB](surveiller-mysql-mariadb.md) : Soyez alerté quand MySQL ou MariaDB s'arrête ou n'accepte plus de connexions, avec les vérifications de service et de port du catalogue MonitorFlow.
- [Comment surveiller Redis](surveiller-redis.md) : Soyez alerté quand Redis s'arrête ou n'accepte plus de connexions sur un serveur Linux, avec les vérifications de service et de port de MonitorFlow.
- [Comment surveiller MongoDB](surveiller-mongodb.md) : Soyez alerté quand MongoDB s'arrête ou n'accepte plus de connexions sur un serveur Linux, avec les vérifications du service mongod et du port 27017.
- [Comment surveiller SQL Server](surveiller-sql-server.md) : Soyez alerté quand une instance SQL Server s'arrête ou refuse les connexions sous Windows, avec les vérifications du service MSSQLSERVER et du port 1433.

## Web et virtualisation

- [Comment surveiller nginx](surveiller-nginx.md) : Soyez alerté quand nginx s'arrête, quand les ports 80 et 443 ne répondent plus ou quand son journal signale des erreurs critiques.
- [Comment surveiller Apache](surveiller-apache.md) : Soyez alerté quand le serveur web Apache s'arrête ou que les ports 80 et 443 ne répondent plus, sous Debian, Ubuntu, RHEL et Rocky.
- [Comment surveiller un site web en HTTPS](surveiller-site-web-https.md) : Sachez quand votre site ne répond plus sur le port 443 ou 80, et vérifiez son code de réponse et l'expiration de son certificat avec un script.
- [Comment surveiller Docker](surveiller-docker.md) : Soyez alerté quand le moteur Docker ou containerd s'arrête sur un serveur Linux, et quand un port publié par un conteneur ne répond plus.
- [Comment surveiller Proxmox VE](surveiller-proxmox-ve.md) : Surveillez vos hôtes Proxmox VE : l'interface web sur le port 8006, le service de cluster, et l'espace disque, le CPU et la mémoire des VM.

## Services Linux

- [Comment surveiller SSH et fail2ban sous Linux](surveiller-ssh-fail2ban.md) : Soyez alerté quand le serveur SSH s'arrête, que le port 22 ne répond plus, que fail2ban cesse de bannir ou que les connexions ratées s'accumulent.
- [Comment surveiller cron et les tâches planifiées sous Linux](surveiller-cron.md) : Soyez alerté quand cron s'arrête sur un serveur Linux, et quand une tâche nocturne, comme une sauvegarde, n'a pas produit son résultat à temps.
- [Comment surveiller la synchronisation de l'heure (NTP) sous Linux et Windows](surveiller-synchronisation-heure.md) : Soyez alerté quand chrony, systemd-timesyncd ou le service de temps Windows s'arrête, et quand l'horloge dérive, avant l'échec des connexions.
- [Comment surveiller Postfix sous Linux](surveiller-postfix.md) : Soyez alerté quand Postfix s'arrête, que le port 25 ne répond plus ou que la file d'attente grossit, pour que le courrier de vos serveurs continue de partir.
- [Comment surveiller les partages de fichiers NFS et SMB](surveiller-partages-nfs-smb.md) : Soyez alerté quand un serveur de fichiers ne répond plus en NFS ou SMB, que son service s'arrête, ou qu'un partage monté sur un client Linux se remplit.

## Windows

- [Comment surveiller les services Windows](surveiller-services-windows.md) : Soyez alerté quand un service Windows s'arrête ou disparaît, comme Windows Update, le spouleur d'impression, le serveur DNS ou le service de temps.
- [Comment surveiller IIS](surveiller-iis.md) : Soyez alerté quand le serveur web IIS s'arrête ou ne répond plus sur les ports 80 et 443 de vos serveurs Windows.
- [Comment surveiller les contrôleurs de domaine Active Directory](surveiller-active-directory.md) : Surveillez vos contrôleurs de domaine : Active Directory, serveur DNS, LDAP sur le port 389, service de temps et erreurs du journal Système.
- [Comment surveiller les erreurs du journal d'événements Windows](surveiller-journal-evenements-windows.md) : Soyez alerté quand Windows écrit un événement critique ou d'erreur dans le journal Système ou Application, sur un serveur ou tout un site.
- [Comment surveiller le Bureau à distance (RDP)](surveiller-bureau-a-distance.md) : Soyez alerté quand le Bureau à distance ne répond plus sur le port 3389 ou que son service s'arrête, sur vos serveurs Windows.

## Administration

- [Comment se connecter à MonitorFlow avec votre propre fournisseur d'identité](connexion-avec-votre-fournisseur.md) : Laissez vos collègues se connecter avec les comptes et le second facteur qu'ils ont déjà : OpenID Connect (Entra ID, Google, Keycloak, authentik) ou un annuaire LDAP en TLS.
