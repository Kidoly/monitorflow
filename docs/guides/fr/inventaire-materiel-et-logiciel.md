---
title: "Comment savoir de quoi est fait chaque serveur et ce qu’il fait tourner"
description: "Voyez le modèle, le numéro de série, les processeurs, la mémoire, les disques, les cartes réseau, le BIOS, les logiciels installés, les services, les fonctionnalités Windows et les licences de chaque serveur, trouvez les serveurs qui ont un vieil OpenSSL, suivez les changements dans le temps et exportez-les en CSV."
slug: inventaire-materiel-et-logiciel
lang: fr
translation: hardware-and-software-inventory
---

# Comment savoir de quoi est fait chaque serveur et ce qu’il fait tourner

Avant un bulletin de sécurité, une demande de garantie ou une réunion de budget, les questions sont les mêmes : quels serveurs ont cette version d’OpenSSL, lesquels tournent sur ce modèle de disque, quand finit la garantie de celui-ci, qu’est-ce qui a changé dessus la semaine dernière. L’agent de MonitorFlow y répond sans rien régler.

## Ce que l’agent lit

À partir de MonitorFlow 0.12, chaque agent envoie l’inventaire de son serveur à son démarrage, quand quelque chose change, et au moins une fois par jour. Il ne fait que lire ; sous Linux il tourne avec son propre utilisateur.

| | Linux | Windows |
| --- | --- | --- |
| Modèle, constructeur, numéro de série, BIOS | oui (depuis le firmware) | oui |
| Processeurs, mémoire totale | oui | oui |
| Barrettes mémoire (emplacement, taille, référence) | non : il faut root pour les lire | oui |
| Disques (modèle, numéro de série, taille, firmware, SSD ou disque dur) | oui | oui |
| Cartes réseau (adresse MAC, vitesse, adresses, pilote) | oui | oui |
| Logiciels installés et versions | les paquets du système | les programmes installés |
| Services et leur démarrage | oui | oui |
| Fonctionnalités Windows, licences | | oui |

Sous Linux, les numéros de série ne sont lisibles que par root : le service de l’agent les copie pour lui à son démarrage, comme il le fait déjà pour l’identifiant matériel. Un agent lancé à la main, ou dans un conteneur, n’en remonte aucun.

## L’inventaire d’un serveur

Ouvrez un serveur et choisissez l’onglet **Inventaire**. Il montre le matériel, les disques, barrettes mémoire et cartes réseau, les logiciels installés avec un filtre et un bouton **Exporter en CSV**, les services, fonctionnalités Windows et licences, et les **Changements** : ce qui a été ajouté, retiré ou modifié, du plus récent au plus ancien, paquets compris. Les changements sont gardés un an.

**Garantie.** Les constructeurs ne donnent la garantie que par des services qui demandent une clé, MonitorFlow ne la cherche donc pas. Un opérateur du site saisit la date de fin et une note (le contrat, le niveau de service) avec **Renseigner…**. Une garantie qui se termine dans les 90 jours est en orange, une garantie expirée en rouge.

## Chercher sur tout le parc

**Inventaire** dans la barre latérale cherche sur tous les serveurs que vous voyez :

- **Logiciels** : saisissez un nom, comme `openssl`, et une version dans **Version inférieure à**, comme `3.0.15`, pour lister les serveurs encore sur une plus ancienne. La recherche couvre les paquets Linux, les mises à jour Windows, les applications winget, Flatpak et Snap, et les programmes Windows. Les versions sont comparées comme le fait Debian, ce qui convient aussi aux versions RPM et Windows.
- **Disques, Modèles et BIOS, Processeurs, Cartes réseau, Services, Fonctionnalités Windows, Licences** : laissez le texte vide pour tout lister, ou saisissez une partie de n’importe quel champ (un modèle de disque, une version de BIOS, un nom de service).

**Exporter en CSV** télécharge tous les résultats, jusqu’à 100 000 lignes, avec une colonne par élément et les détails sous forme de paires `nom=valeur`.

## Par l’API

Tout ce qui précède est dans l’API publique (tag `inventory`) : `GET /api/v1/servers/{id}/inventory`, `GET /api/v1/servers/{id}/inventory/changes`, `PUT /api/v1/servers/{id}/warranty` et `GET /api/v1/inventory?kind=software&q=openssl&version_below=3.0.15` (ajoutez `format=csv` pour le fichier).

## S’il manque quelque chose

- **« Pas encore d’inventaire »** : l’agent est antérieur à la version 0.12, ou ne s’est pas reconnecté depuis sa mise à jour. Il envoie son inventaire dès qu’il se connecte.
- **Pas de numéro de série sous Linux** : l’agent ne tourne pas depuis le service de MonitorFlow, ou le firmware n’en a pas (beaucoup de machines virtuelles donnent une valeur bidon, qui est écartée).
- **Pas de barrettes mémoire sous Linux** : les lire demande root ; la mémoire totale est affichée à la place.
