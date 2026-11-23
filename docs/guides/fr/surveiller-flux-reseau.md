---
title: "Comment voir quelles adresses et applications utilisent votre bande passante (NetFlow, sFlow, IPFIX)"
description: "Recevez NetFlow, sFlow ou IPFIX de vos routeurs, pare-feu et switchs sur un relais ou le serveur central, voyez les plus grosses adresses, applications et interfaces dans le temps, et soyez alerté quand une adresse en utilise trop."
slug: surveiller-flux-reseau
lang: fr
translation: monitor-traffic-flows
---

# Comment voir quelles adresses et applications utilisent votre bande passante (NetFlow, sFlow, IPFIX)

SNMP vous dit qu’un lien est plein. Les flux réseau vous disent **qui** le remplit et **avec quoi** : les adresses, les applications (HTTPS, SMB, RDP…) et les interfaces. Les routeurs, pare-feu et beaucoup de switchs les exportent en NetFlow, sFlow ou IPFIX ; MonitorFlow les reçoit sur le **relais du site** ou sur le **serveur central**.

## Pourquoi c’est important

Un lien d’agence saturé ralentit tout le monde, et le graphe de l’interface dit seulement « plein ». Une sauvegarde lancée au mauvais moment, un poste qui synchronise un disque en ligne ou une machine compromise qui envoie des données se ressemblent en SNMP et se distinguent très bien dans les flux.

## Ce que MonitorFlow montre

| Vue | Ce qu’elle dit |
| --- | --- |
| **Totaux** | Le débit moyen et le volume de la période |
| **Graphe** | Les 5 plus grosses sources, destinations, applications, protocoles ou interfaces dans le temps, empilés, et le reste |
| **Sources et destinations** | Les adresses qui ont le plus envoyé et reçu |
| **Applications** | Le protocole et le port du service, nommés quand ils sont connus : `tcp/443` est HTTPS |
| **Protocoles** | TCP, UDP, ICMP, GRE… |
| **Interfaces d’entrée et de sortie** | Les interfaces de l’équipement, nommées par sa vérification SNMP des interfaces |

Cliquez sur une adresse pour ne voir que son trafic dans toutes les vues. Les périodes vont de 1 heure à 30 jours. Chaque minute est gardée 48 heures, puis chaque heure 31 jours.

NetFlow v5 et v9, IPFIX et sFlow v5 sont lus. Quand l’équipement échantillonne (1 paquet sur 1 000 par exemple), MonitorFlow multiplie par le taux qu’il annonce : les chiffres sont ce qui est passé sur le lien.

## L’activer

1. Ajoutez le routeur, le pare-feu ou le switch comme équipement s’il ne l’est pas encore (voir [équipements réseau](surveiller-equipements-reseau.md)). Sa vérification SNMP des interfaces nomme ses interfaces dans les flux.
2. Ouvrez la page de l’équipement, onglet **Flux**.
3. **Reçus par** : le relais de son site est choisi s’il y en a un. **Adresse source** : seulement si l’équipement exporte depuis une autre adresse que la sienne, comme une interface de bouclage.
4. Cliquez sur **« Recevoir les flux »**.
5. Sur l’équipement, exportez les flux vers l’adresse de ce relais (ou du serveur central), sur le port UDP **2055** (NetFlow), **6343** (sFlow) ou **4739** (IPFIX). Chaque port accepte tous les formats.

Le relais n’ouvre ces ports que tant qu’un équipement lui envoie des flux, et n’accepte que les datagrammes de ces équipements. Autorisez-les dans les pare-feu entre les équipements et le relais. La première minute apparaît environ une minute après le début de l’envoi.

## Être alerté

Sous les vues, **Alerte sur une adresse** ajoute une alerte en un clic : « au-dessus de 100 Mbit/s pendant 5 minutes » ouvre un incident quand une adresse, quelle qu’elle soit, envoie ou reçoit plus que cela pendant toute la durée. C’est un seuil de l’équipement, sur la métrique « Trafic d’une adresse » : vous pouvez le modifier, le mettre en silence ou ajouter un niveau critique dans l’onglet **Capteurs** comme les autres.

## Quand rien n’apparaît

- **« Rien reçu pour l’instant »** : vérifiez sur l’équipement que l’exportateur vise l’adresse du relais et le bon port, et qu’aucun pare-feu ne bloque l’UDP.
- **Une autre adresse** : certains équipements exportent depuis une interface de bouclage ou d’administration. Indiquez-la comme **Adresse source**.
- **Interfaces affichées « ifIndex 7 »** : l’équipement n’a pas de vérification SNMP des interfaces, ou elle n’a pas encore tourné.
- **En venant de PRTG** : l’importeur liste les capteurs NetFlow, sFlow et IPFIX avec ce rappel ; activez plutôt les Flux sur chaque équipement.
