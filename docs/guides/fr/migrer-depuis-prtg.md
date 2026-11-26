---
title: "Comment migrer de PRTG vers MonitorFlow"
description: "Lisez vos équipements, capteurs, limites, fenêtres de maintenance et notifications PRTG, voyez ce qui sera repris, et créez en un clic les serveurs, contrôles, seuils, silences et canaux correspondants."
slug: migrer-depuis-prtg
lang: fr
translation: move-from-prtg
---

# Comment migrer de PRTG vers MonitorFlow

Ressaisir des centaines d'équipements, de capteurs et de limites est la partie la plus pénible d'un changement d'outil. MonitorFlow lit votre configuration PRTG, par son API ou depuis son fichier de configuration, et montre ce qu'il créerait avant de créer quoi que ce soit. PRTG est seulement lu : rien n'y est modifié, et il peut continuer à tourner à côté de MonitorFlow jusqu'à la bascule.

## Ce qui est repris, et comment

| Dans PRTG | Dans MonitorFlow | Fidélité |
| --- | --- | --- |
| Équipement, avec son adresse | Serveur en attente de son agent, dans le site choisi | Exacte. Les groupes de l'équipement deviennent ses groupes |
| Sonde et groupe | Groupes des serveurs en dessous | Exacte |
| Capteur Ping | L'alerte hors ligne de chaque serveur | Approchée : MonitorFlow sait qu'un serveur est tombé quand son agent ne répond plus ; les temps de ping ne sont pas mesurés |
| Capteur de charge processeur, avec des limites sur son canal Total | Seuil d'utilisation processeur du serveur | Exacte |
| Capteur de mémoire, avec des limites sur son canal de pourcentage disponible | Seuil de mémoire disponible | Exacte |
| Capteurs d'espace disque et de volume, avec des limites sur un canal de pourcentage libre | Seuil d'espace disque pour ce lecteur ou point de montage | Approchée : lu comme espace utilisé |
| Capteur de service Windows | Contrôle du service Windows, critique quand il s'arrête | Exacte |
| Capteur de port | Contrôle de port sur l'adresse de l'équipement | Approchée : vérifié par l'agent du serveur, pas depuis une sonde |
| Capteurs Bureau à distance, SMTP, IMAP et POP3 | Contrôle de port de leur port sur l'adresse de l'équipement | Approchée : MonitorFlow vérifie que le port accepte les connexions, sans parler le protocole |
| Capteurs HTTP et HTTP avancé | Contrôle de site web de la page : sa méthode, le mot-clé qu'elle doit ou ne doit pas contenir, les en-têtes personnalisés et le délai, avec les limites de temps de réponse en seuil | Exacte ; approchée pour un POST, un mot-clé donné en expression régulière ou une authentification Windows. Le mot de passe n'est pas importé : saisissez-le ensuite dans le contrôle |
| Capteur de certificat SSL | Contrôle de site web de son hôte et de son port, avec ses limites en jours sur les jours restants. Une page du même site déjà contrôlée les reprend à sa place | Approchée : le contrôle demande aussi la page, quelle que soit sa réponse |
| Capteurs de journal d'événements | Contrôle du journal d'événements avec le même journal, les mêmes niveaux, sources et ID d'événement | Exacte, ou approchée quand un filtre sur le message ou une limite de nombre ne peut pas être repris |
| Capteurs PostgreSQL, MySQL v2 et MySQL | Contrôle PostgreSQL ou MySQL de l'adresse de l'équipement, avec ses alertes par défaut : exécuté par l'agent du serveur, ou depuis une sonde pour un équipement | Approchée : la requête propre au capteur n'est pas exécutée, seulement la connexion, le temps de réponse et les mesures habituelles de la base. Le mot de passe n'est pas importé : saisissez-le sur le contrôle |
| Capteur Microsoft SQL v2 | Contrôle SQL Server de l'adresse et du port de l'équipement, avec ses alertes par défaut, exécuté comme le contrôle PostgreSQL | Approchée : la requête propre au capteur n'est pas exécutée, seulement la connexion, le temps de réponse et les mesures habituelles de SQL Server. Le mot de passe n'est pas importé : saisissez-le sur le contrôle |
| Capteurs WMI Microsoft SQL Server | Contrôle SQL Server de l'agent du serveur, avec ses alertes par défaut | Approchée : le contrôle se connecte à SQL Server au lieu de lire ses compteurs par WMI. Créez l'identifiant que décrit le [guide SQL Server](surveiller-sql-server.md) et saisissez son mot de passe sur le contrôle |
| Capteur Docker Container Status | Contrôle Docker de l'agent du serveur, limité à ce conteneur, avec ses alertes par défaut | Exacte ; approchée, tous les conteneurs, quand le nom du conteneur ne peut pas être lu |
| Capteurs HTTP, HTTP Advanced, HTTP Data Advanced, REST Custom et HTTP XML/REST Value dont l'URL est celle de Jolokia (`/jolokia/`) | Contrôle Java (JMX) de cette JVM, à l'adresse, au port et au chemin de l'URL, avec ses alertes par défaut : heap, ramasse-miettes, threads, et ceux de Tomcat ou Kafka s'ils tournent | Approchée : les mesures habituelles de la JVM remplacent la valeur lue par le capteur. Le mot de passe n'est pas importé : saisissez-le sur le contrôle si Jolokia en demande un |
| Fenêtre de maintenance | Silence des serveurs qu'elle couvre | Exacte ; 30 jours au plus |
| Équipement, groupe ou sonde en pause | Silence de 30 jours, le plus long possible | Approchée |
| Modèle de notification par e-mail | Canal e-mail | Approchée : il reçoit chaque incident, alors que PRTG ne l'utilisait que là où un déclencheur le nommait. Son serveur de messagerie n'est pas dans l'API de PRTG : ajoutez un canal e-mail avec le vôtre si l'aperçu le demande |
| Modèle de notification qui appelle une URL | Canal Slack, Teams, Discord ou webhook, selon l'URL | Approchée : MonitorFlow envoie son propre message |

Ce qui ne peut pas être repris est listé aussi, avec la raison : capteurs de trafic SNMP, équipements surveillés seulement en SNMP (commutateurs, imprimantes), capteurs d'un type sans contrôle équivalent dans MonitorFlow (dont Oracle et ADO SQL), limites de canaux sans seuil équivalent, capteurs mis en pause par un horaire ou une dépendance, un capteur seul en pause (MonitorFlow met en silence des serveurs entiers), et notifications par SMS ou push. Les capteurs NetFlow, sFlow et IPFIX sont listés avec ce qu’il faut faire à la place : activer l’onglet **Flux** de l’équipement ([flux réseau](surveiller-flux-reseau.md)).

## Avant de commencer

Il faut être administrateur du site où vont les serveurs, et avoir l'un de ces accès :

- **Un jeton d'API** d'un utilisateur PRTG qui voit les équipements à reprendre. Dans PRTG, ouvrez **Setup › Account Settings › API Keys** et ajoutez une clé en lecture.
- **Un nom d'utilisateur et son passhash.** Dans PRTG, ouvrez **Setup › Account Settings › My Account** et cliquez sur **Show passhash**.
- **Le fichier de configuration**, `PRTG Configuration.dat`, copié depuis `C:\ProgramData\Paessler\PRTG Network Monitor\` sur le serveur central PRTG. Il contient tous les réglages, limites des canaux comprises ; ses heures de maintenance sont lues en UTC, car le fichier ne dit pas le fuseau horaire de PRTG.

Si PRTG est derrière un proxy inverse ou un pare-feu applicatif web qui demande un utilisateur et un mot de passe (authentification HTTP basique) avant la connexion de PRTG, remplissez aussi **Utilisateur du proxy** et **Mot de passe du proxy** (`proxy_user` et `proxy_password` dans l'API). Les deux accompagnent chaque requête, avec le jeton ou le passhash.

Le jeton, le passhash et le mot de passe du proxy ne servent qu'à cette lecture : MonitorFlow ne les garde jamais et ne les écrit dans aucun journal. Utilisez une adresse `https://` pour qu'il ne circule pas en clair.

## Lire et voir l'aperçu

1. Ouvrez la page d'import et choisissez **PRTG**.
2. Saisissez l'adresse de PRTG, comme `https://prtg.example.com`, et le jeton, ou l'utilisateur et le passhash. Ou envoyez plutôt le fichier de configuration.
3. Choisissez le site où vont les serveurs.

MonitorFlow lit les sondes, groupes, équipements et capteurs, les limites des canaux qu'il peut reprendre, les fenêtres de maintenance et les modèles de notification. Une grosse installation prend une minute : les requêtes partent quelques-unes à la fois pour ne pas ralentir PRTG.

L'aperçu liste tout en trois parties : ce qui est repris exactement, ce qui est approché (avec la différence) et ce qui ne peut pas être repris (avec la raison). Décochez ce que vous ne voulez pas : décocher un serveur décoche ses contrôles, seuils et silences.

## Confirmer, puis installer les agents

Cliquez sur **Confirmer**. Les serveurs apparaissent dans le parc en attente de leur agent, avec la commande d'installation ; leurs contrôles, seuils et silences sont des réglages ordinaires que vous pouvez modifier. Installez l'agent sur chaque machine : il prend le serveur de son nom d'hôte, et la surveillance commence.

Vous changez d'avis ? **Annuler** retire ce que l'import a créé, sauf les serveurs qu'un agent a déjà pris.

## Avec l'API

Tout ce qui précède existe dans l'API publique, avec une clé d'API d'un administrateur du site :

```bash
# Les outils qu'un import sait lire, et les champs de leur connexion.
curl -H "Authorization: Bearer $MF_KEY" https://monitorflow.example.com/api/v1/imports/sources

# Lire PRTG et obtenir l'aperçu.
curl -H "Authorization: Bearer $MF_KEY" -H "Content-Type: application/json" \
  -d '{"source": "prtg", "site_id": "…", "connection": {"url": "https://prtg.example.com", "apitoken": "…"}}' \
  https://monitorflow.example.com/api/v1/imports

# Le confirmer, en laissant de côté des éléments par leur ID.
curl -H "Authorization: Bearer $MF_KEY" -H "Content-Type: application/json" \
  -d '{"skip": []}' https://monitorflow.example.com/api/v1/imports/<id>/confirm
```

Pour lire le fichier de configuration, envoyez `"files": [{"name": "PRTG Configuration.dat", "content": "<base64>", "base64": true}]` à la place de `connection`.

## En cas de problème

- **« refused the credentials »** : le jeton ou le passhash est faux, ou l'utilisateur n'a pas accès à l'API.
- **« answered with a redirect »** : PRTG renvoie le HTTP vers le HTTPS ; saisissez l'adresse en `https://`.
- **Un équipement manque** : l'utilisateur PRTG ne le voit pas. Lisez avec un utilisateur qui voit tous les équipements à reprendre.
- **Un capteur est approché ou ne peut pas être repris** : l'aperçu dit pourquoi. Ajoutez ce qu'il vous faut à la main depuis l'onglet **Capteurs** du serveur après l'import.
