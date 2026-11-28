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

## Passer à MonitorFlow

- [Passer de Zabbix ou Prometheus à MonitorFlow](venir-d-un-autre-outil.md) : Importez vos hôtes, vérifications, seuils, maintenances et canaux de notification depuis Zabbix ou Prometheus, prévisualisez ce qui est repris, puis installez l'agent.
- [Comment migrer de PRTG vers MonitorFlow](migrer-depuis-prtg.md) : Lisez vos équipements, capteurs, limites, fenêtres de maintenance et notifications PRTG, voyez ce qui sera repris, et créez en un clic les serveurs, contrôles, seuils, silences et canaux correspondants.

## Serveurs

- [Comment être alerté quand un serveur tombe](alerte-serveur-hors-ligne.md) : Sachez en quelques minutes qu'un serveur Linux ou Windows ne répond plus, sans rien configurer, et soyez prévenu par e-mail ou messagerie.
- [Comment surveiller le processeur et la mémoire de serveurs Linux et Windows](surveiller-cpu-memoire.md) : Soyez alerté quand le processeur d'un serveur reste saturé ou que sa mémoire s'épuise, avec des seuils intégrés modifiables par serveur ou par site.
- [Comment surveiller l'espace disque de serveurs Linux et Windows](surveiller-espace-disque.md) : Soyez prévenu avant qu'un disque se remplisse et arrête vos services : alertes par disque à 85 % et 95 % d'office, et alertes de disque lent.
- [Comment surveiller les erreurs des fichiers journaux sous Linux et Windows](surveiller-fichiers-journaux.md) : Soyez alerté quand une erreur, un échec de connexion ou un motif choisi apparaît dans un fichier journal, et lisez les journaux système au même endroit.
- [Comment tout surveiller avec votre propre script (Bash ou PowerShell)](surveiller-avec-votre-script.md) : Transformez un script shell ou PowerShell en vérification avec alerte : codes de sortie pour OK, avertissement et critique, lignes nom=valeur en métriques.
- [Comment surveiller switchs, imprimantes, onduleurs et autres équipements réseau](surveiller-equipements-reseau.md) : Surveillez ce qui ne peut pas faire tourner l’agent : ping, ports TCP et UDP, et SNMP v2c ou v3 pour les interfaces, le trafic, la batterie des onduleurs, les températures et les consommables d’imprimante.
- [Comment trouver ce qui est sur votre réseau](analyser-le-reseau.md) : Analysez une plage d’adresses depuis le serveur central ou un relais, voyez quelles machines peuvent recevoir l’agent et quels équipements surveiller, et ajoutez-les en un clic.
- [Comment lancer des scripts sur vos serveurs et automatiser les correctifs](lancer-des-scripts-et-automatiser.md) : Gardez vos scripts shell et PowerShell dans MonitorFlow, lancez-les sur un serveur, un service métier ou un site entier, tout de suite ou cette nuit, lisez la sortie de chaque serveur, et lancez-les tout seuls quand un incident s’ouvre.
- [Comment dessiner une carte de votre réseau en direct](dessiner-des-cartes-reseau.md) : Obtenez la carte d’un site ou d’un client dessinée à partir de vos relais, de vos dépendances et des voisins LLDP et CDP de vos commutateurs, colorée par les incidents et affichée sur un écran mural.
- [Comment voir quelles adresses et applications utilisent votre bande passante (NetFlow, sFlow, IPFIX)](surveiller-flux-reseau.md) : Recevez NetFlow, sFlow ou IPFIX de vos routeurs, pare-feu et switchs sur un relais ou le serveur central, voyez les plus grosses adresses, applications et interfaces dans le temps, et soyez alerté quand une adresse en utilise trop.
- [Comment savoir de quoi est fait chaque serveur et ce qu’il fait tourner](inventaire-materiel-et-logiciel.md) : Voyez le modèle, le numéro de série, les disques, le BIOS, les logiciels installés, les services et les licences de chaque serveur, trouvez les serveurs qui ont un vieil OpenSSL, suivez les changements et exportez-les en CSV.

## Bases de données

- [Comment surveiller PostgreSQL](surveiller-postgresql.md) : Suivez les connexions, transactions longues, verrous, cache et réplication de PostgreSQL, et soyez alerté quand il ne répond plus, avec le contrôle PostgreSQL prêt à l'emploi de MonitorFlow.
- [Comment surveiller MySQL et MariaDB](surveiller-mysql-mariadb.md) : Suivez les connexions, requêtes lentes, le buffer pool InnoDB et la réplication de MySQL et MariaDB, et soyez alerté quand la base ne répond plus, avec le contrôle prêt à l'emploi de MonitorFlow.
- [Comment surveiller Redis](surveiller-redis.md) : Suivez la mémoire, les évictions, les clients, les sauvegardes et la réplication de Redis, et soyez alerté quand il ne répond plus, avec le contrôle Redis prêt à l'emploi de MonitorFlow.
- [Comment surveiller MongoDB](surveiller-mongodb.md) : Soyez alerté quand MongoDB s'arrête ou n'accepte plus de connexions sur un serveur Linux, avec les vérifications du service mongod et du port 27017.
- [Comment surveiller SQL Server](surveiller-sql-server.md) : Suivez les connexions, lots de requêtes, la durée de vie des pages, les blocages, la taille des bases et des journaux et les réplicas Always On de SQL Server, et soyez alerté quand il ne répond plus, avec le contrôle prêt à l'emploi de MonitorFlow.

## Web et virtualisation

- [Comment surveiller nginx](surveiller-nginx.md) : Soyez alerté quand nginx s'arrête, quand les ports 80 et 443 ne répondent plus ou quand son journal signale des erreurs critiques.
- [Comment surveiller Apache](surveiller-apache.md) : Soyez alerté quand le serveur web Apache s'arrête ou que les ports 80 et 443 ne répondent plus, sous Debian, Ubuntu, RHEL et Rocky.
- [Comment surveiller un site web en HTTPS](surveiller-site-web-https.md) : Sachez quand votre site ne répond plus comme il le devrait : son code de réponse, le texte de la page, son temps de réponse, ses redirections et l'expiration de son certificat.
- [Comment surveiller Docker](surveiller-docker.md) : Suivez chaque conteneur Docker ou Podman : s'il tourne, ses redémarrages, son contrôle de santé, son processeur et sa mémoire, et soyez alerté quand le moteur ou un conteneur s'arrête.
- [Comment surveiller une application Java (JMX, Tomcat, Kafka)](surveiller-java-jmx.md) :Suivez la heap, le ramasse-miettes, les threads et les interblocages d'une JVM, les connecteurs de Tomcat et les partitions d'un broker Kafka via Jolokia, et soyez alerté quand elle manque de mémoire ou ne répond plus, avec le contrôle Java prêt à l'emploi de MonitorFlow.
- [Comment surveiller Kubernetes](surveiller-kubernetes.md) : Suivez un cluster Kubernetes par son API avec un jeton en lecture seule : les nœuds et leurs conditions, les pods qui ne tournent pas ou redémarrent, les déploiements sans assez de réplicas prêts et les volume claims en attente.
- [Comment surveiller Proxmox VE](surveiller-proxmox-ve.md) : Surveillez vos hôtes Proxmox VE : l'interface web sur le port 8006, le service de cluster, et l'espace disque, le CPU et la mémoire des VM.
- [Comment surveiller VMware vSphere et Hyper-V](surveiller-vmware-hyper-v.md) : Surveillez les hôtes, clusters, machines virtuelles et banques de données d'un vCenter, d'un hôte ESXi ou d'un hôte Hyper-V, et reliez chaque machine virtuelle à son serveur.
- [Comment surveiller les machines virtuelles AWS, Azure et Google Cloud](surveiller-aws-azure.md) : Surveillez les instances EC2 d'un compte AWS, les machines virtuelles d'un abonnement Azure et les instances Compute Engine d'un projet Google Cloud : leur état, leurs contrôles d'état, leur processeur, leur réseau et leurs disques, avec des identifiants en lecture seule, et reliez chacune à son serveur.

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
- [Comment recevoir les alertes MonitorFlow sur votre téléphone](alertes-sur-votre-telephone.md) : Installez MonitorFlow sur l’écran d’accueil de votre téléphone, recevez des notifications push pendant l’astreinte, et prenez en charge ou mettez en silence une alerte en deux gestes, sans store ni service extérieur.
