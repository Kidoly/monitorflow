---
title: "Comment trouver ce qui est sur votre réseau"
description: "Analysez une plage d’adresses depuis le serveur central ou un relais, voyez quelles machines peuvent recevoir l’agent et quels équipements surveiller, et ajoutez-les en un clic. Une analyse peut tourner chaque jour et dire ce qui est nouveau."
slug: analyser-le-reseau
lang: fr
translation: scan-the-network
---

# Comment trouver ce qui est sur votre réseau

Reprendre un réseau vient rarement avec la liste complète de ce qui s’y trouve. MonitorFlow peut chercher pour vous : donnez-lui une plage d’adresses, il liste ce qui répond, avec ce qu’il propose pour chaque adresse. Rien n’est ajouté sans votre accord.

## Ce que fait une analyse

Depuis le **serveur central** ou un **relais du site** (le relais par défaut, puisqu’il est sur le réseau analysé), MonitorFlow essaie chaque adresse de la plage :

- un **ping** ;
- une **connexion TCP** à quelques ports : par défaut 22 (SSH), 23, 80, 135, 139, 443, 445, 3389 (RDP), 5985 (WinRM), 8080 et 8443 ;
- **SNMP**, avec les identifiants des équipements déjà surveillés sur ce site et ceux donnés à l’analyse ;
- le **nom** donné par le DNS inverse et, sur le réseau même de la sonde, l’**adresse MAC**.

Elle reste discrète : 50 essais par seconde au plus, une seconde d’attente chacun, si bien qu’un /24 prend environ deux minutes. Elle ne sort jamais de la plage et n’analyse jamais de noms, seulement des adresses IP.

## Lancer une analyse

1. Ouvrez **Parc** et cliquez sur **« Analyser le réseau »**, puis **« Nouvelle analyse »**.
2. Donnez-lui un nom et les **adresses** : `192.168.1.0/24`, une plage comme `192.168.1.10-50` ou des adresses seules, séparées par des virgules ou des lignes (1 024 adresses au plus).
3. Gardez **Analysé depuis** tel que proposé, ou choisissez le serveur central.
4. Donnez si besoin des identifiants SNMP à essayer, et choisissez d’**analyser à nouveau** toutes les heures, tous les jours ou toutes les semaines.
5. Cliquez sur **« Analyser maintenant »**. La page suit l’analyse en cours.

## Ce qu’elle propose

| Ce qui a répondu | Proposé |
| --- | --- |
| SSH ouvert | **Installer l’agent** : un serveur Linux ou Unix. Le bouton ouvre la commande d’installation du site |
| RDP ou WinRM ouvert | **Installer l’agent** : une machine Windows |
| SNMP, et c’est un switch, un routeur, un pare-feu, une imprimante ou un onduleur | **Ajouter comme équipement** avec les contrôles et alertes de son type (son modèle), même si SSH est ouvert |
| SNMP sinon | **Ajouter comme équipement** avec le modèle suggéré par la détection, que vous pouvez changer |
| Autre chose | **Ajouter comme équipement** avec son ping et un contrôle par port ouvert |

Ce que le site surveille déjà est listé sous **Déjà surveillé**, avec la façon dont il a été reconnu : un équipement à la même adresse, ou un serveur dont l’agent déclare la même adresse MAC ou porte le même nom. Il n’est plus jamais proposé.

Cliquez sur **Ajouter** sur une ligne, ou sur **« Ajouter les équipements proposés »** pour tous. Un équipement est contrôlé depuis le même endroit que l’analyse. **Écarter** cesse de proposer une adresse pour cette analyse ; **« Afficher les écartés »** la fait revenir.

## Les analyses qui tournent seules

Une analyse quotidienne compare ce qui a répondu avec l’analyse précédente : les nouvelles adresses sont marquées **Nouveau**, celles qui ne répondent plus sont listées comme disparues. Cochez **« Envoyer une notification quand une nouvelle adresse répond ou qu’une adresse a disparu »** pour l’apprendre sur les canaux de notification du site. C’est désactivé par défaut.

## Bon à savoir

- Un relais déconnecté garde son analyse en attente et la lance à son retour.
- Un relais plus ancien que cette fonction indique qu’il ne mène pas encore d’analyses : mettez-le à jour, comme sur la page Relais.
- Les adresses MAC, et donc la reconnaissance d’un serveur par elles, ne se voient que sur le réseau même de la sonde.
- Par l’API, tout cela est sous `/api/v1/network-scans` (voir la documentation de l’API dans Paramètres › Clés d’API).
