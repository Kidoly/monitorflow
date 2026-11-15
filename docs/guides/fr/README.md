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
