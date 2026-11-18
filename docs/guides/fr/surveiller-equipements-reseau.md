---
title: "Comment surveiller switchs, imprimantes, onduleurs et autres équipements réseau"
description: "Surveillez ce qui ne peut pas faire tourner l’agent : ping, ports TCP et UDP, et SNMP v2c ou v3 pour les interfaces, le trafic, la batterie des onduleurs, les températures et les consommables d’imprimante, vérifiés depuis un relais du site ou le serveur central."
slug: surveiller-equipements-reseau
lang: fr
translation: monitor-network-devices
---

# Comment surveiller switchs, imprimantes, onduleurs et autres équipements réseau

Tout ne peut pas faire tourner l’agent MonitorFlow. Un switch, un routeur, une imprimante, un onduleur ou un NAS méritent quand même d’être surveillés, tout comme une adresse vue depuis ailleurs : une agence qui perd son lien paraît normale de l’intérieur et injoignable depuis le serveur central. MonitorFlow les vérifie par le réseau, depuis le **relais de leur site** ou depuis le **serveur central**, et alerte comme pour un serveur.

## Pourquoi c’est important

Un port de switch qui tombe coupe les serveurs derrière lui ; un onduleur sur batterie vous laisse des minutes, pas des heures ; une imprimante sans toner arrête un bureau. Aucun ne prévient tout seul.

## Ce que MonitorFlow vérifie

| Contrôle | Ce qu’il mesure | Alerte par défaut |
| --- | --- | --- |
| **Ping** | Si l’adresse répond, l’aller-retour et la perte de paquets sur quelques pings | Critique après 2 minutes sans réponse, avertissement au-delà de 20 % de perte pendant 5 minutes |
| **Port TCP** | Si un port accepte les connexions | Critique quand il se ferme |
| **Port UDP** | Un serveur DNS ou NTP qui répond ; pour les autres ports, que l’équipement ne refuse pas le paquet | Critique quand il ne répond plus |
| **SNMP** | Temps de fonctionnement, interfaces actives ou non, trafic entrant et sortant, erreurs et rejets ; processeur, mémoire et stockage ; batterie de l’onduleur, autonomie et passage sur batterie ; températures ; consommables d’imprimante | Selon le type d’équipement, ci-dessous |

Un port UDP qui n’est ni DNS ni NTP n’a pas de réponse à attendre : MonitorFlow le dit disponible tant que l’équipement ne le refuse pas. Un port qui reste muet et un port bloqué par un pare-feu se ressemblent.

## Ajouter un équipement

1. Ouvrez **Parc** et cliquez sur **« Ajouter un équipement »**.
2. Donnez-lui un **Nom** et son **Adresse** (une adresse IP ou un nom DNS).
3. **Vérifié depuis** : le relais de son site est choisi quand il y en a un. Choisissez le serveur central pour le voir depuis l’extérieur.
4. Laissez **« Le pinguer chaque minute… »** coché pour un ping avec ses alertes.
5. Pour SNMP, cochez **« Le surveiller en SNMP »**, saisissez la communauté (SNMP v2c) ou l’utilisateur et les mots de passe d’authentification et de chiffrement (SNMP v3, SHA et AES), puis cliquez sur **« Détecter »** : MonitorFlow demande à l’équipement ce qu’il remonte et choisit son **Type d’équipement**.
6. Cliquez sur **« Ajouter l’équipement »**.

L’équipement a sa propre page, avec ses contrôles, leurs valeurs et leurs alertes, et peut être mis en silence ou placé sur un tableau de bord comme un serveur. Il n’apparaît jamais hors ligne ni en attente d’un agent : ses contrôles disent ce qui ne va pas.

## Ce que reçoit chaque type d’équipement

| Type | Contrôles | Alertes |
| --- | --- | --- |
| Switch | SNMP, interfaces, températures | Plus de réponse SNMP ; un lien au-delà de 90 % pendant 10 minutes ; plus d’une erreur par seconde ; au-delà de 70 °C, critique au-delà de 80 °C |
| Routeur, Pare-feu | SNMP, interfaces, processeur et mémoire, températures | Les mêmes, et le processeur au-delà de 90 % |
| NAS | SNMP, interfaces, processeur, mémoire et stockage, températures | Stockage au-delà de 85 %, critique au-delà de 95 % |
| Onduleur | SNMP, batterie de l’onduleur | Critique sur batterie, batterie faible, moins de 10 minutes d’autonomie |
| Imprimante | SNMP, consommables de l’imprimante | Un consommable sous 10 %, critique sous 2 % |
| Générique | SNMP, interfaces | Plus de réponse SNMP ; un lien au-delà de 90 % |

Une interface inactive ne déclenche rien par défaut, puisque les ports inutilisés d’un switch le sont. Pour être prévenu quand un port précis tombe, ajoutez une alerte sur cette interface depuis la page de l’équipement.

## Changer les identifiants

Sur la page de l’équipement, ouvrez **Détails** : la ligne **SNMP** donne la version, l’utilisateur et le port, jamais un mot de passe. **« Modifier… »** les remplace ; un mot de passe laissé vide garde celui enregistré.

## Quand l’alerte se déclenche

- **Plus de réponse SNMP** : vérifiez la communauté ou les mots de passe v3, et que l’équipement accepte SNMP depuis le relais ou le serveur central.
- **Le ping échoue depuis le relais mais pas depuis le serveur central** (ou l’inverse) : le problème est sur le réseau entre les deux, pas sur l’équipement.
- **Sur batterie** : l’onduleur a perdu le secteur. Son autonomie restante dit combien de temps il vous reste.

## Le trafic de vos propres serveurs

Les serveurs avec l’agent mesurent aussi le trafic, les erreurs et les pertes de leurs propres interfaces réseau, sans rien ajouter : leur onglet **Capteurs** a une ligne par interface.
