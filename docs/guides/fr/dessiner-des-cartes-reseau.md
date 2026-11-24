---
title: "Comment dessiner une carte de votre réseau en direct"
description: "Obtenez la carte d’un site ou d’un client dessinée à partir de vos relais, de vos dépendances et des voisins LLDP et CDP de vos commutateurs, colorée par les incidents, arrangée à la main sur votre plan et affichée sur un écran mural."
slug: dessiner-des-cartes-reseau
lang: fr
translation: draw-network-maps
---

# Comment dessiner une carte de votre réseau en direct

Une carte réseau montre ce qui est relié à quoi, aux couleurs du moment : le commutateur en rouge, les serveurs derrière lui injoignables, le lien montant à 95 %. MonitorFlow la dessine à partir de ce qu’il sait déjà, et vous l’arrangez comme vous voulez.

## Créer une carte

1. Ouvrez **Cartes réseau** dans la barre latérale et cliquez sur **« Nouvelle carte »**.
2. Donnez-lui un nom et choisissez de quoi elle est la **carte** : un site, ou un client avec tous ses sites.
3. Cliquez sur **« Créer la carte »**. Tous les serveurs, équipements et relais de ce lieu sont dessinés et disposés d’un coup.

Les liens viennent :

- des **relais** : le serveur central, ses relais, et ce que chaque relais transporte ou vérifie ;
- des **dépendances** que vous avez définies ou importées (« ces serveurs sont derrière ce commutateur ») ;
- des **voisins** : ce que vos commutateurs et routeurs signalent en LLDP et CDP ;
- si vous le voulez, des **adresses trouvées par les analyses du réseau** que rien ne surveille encore, en gris.

## Lire les voisins de vos commutateurs

Les équipements surveillés en SNMP peuvent dire ce qui est branché sur chacun de leurs ports. Dans l’éditeur de carte, cliquez sur **« Lire les voisins maintenant »** : MonitorFlow interroge chaque équipement SNMP du lieu, depuis le relais ou le serveur central qui le vérifie. Un voisin qui est un serveur ou un équipement de la carte devient un lien entre eux, avec le trafic du port du commutateur ; les autres sont dessinés en gris, « Non surveillé ». Relisez-les après un recâblage.

## L’arranger

Cliquez sur **« Modifier la carte »**, puis :

- **faites glisser** les nœuds où vous voulez ; **« Disposer à nouveau »** repart de zéro ;
- **« Ajouter une zone »** pour un étage, une salle ou une baie : déplacez-la, redimensionnez-la par son coin, nommez-la et colorez-la ;
- **« Ajouter une étiquette »** pour n’importe quel texte ;
- **« Tracer un lien »**, puis cliquez sur deux nœuds : donnez-lui une étiquette et, pour un port de commutateur ou une interface de serveur, l’interface dont il montre le trafic ;
- sélectionnez un nœud et **« Retirer de la carte »** pour le masquer ; la liste du bas le remet ;
- ajoutez un **arrière-plan** : un plan ou une photo, PNG, JPEG ou WebP jusqu’à 1 Mio, avec son opacité.

Cliquez sur **« Enregistrer »**. Les serveurs ajoutés plus tard sont disposés sous ce que vous avez placé.

## Lire les couleurs

- Un nœud a la couleur de son état : rouge critique, orange avertissement, gris hors ligne, injoignable ou non surveillé, vert OK.
- Un lien est aussi mauvais que la pire de ses deux extrémités : rouge si l’une est critique, hors ligne ou injoignable ou si son interface est coupée, orange sur un avertissement ou quand l’interface est utilisée à 90 % ou plus.
- Un lien avec une interface montre son trafic et est d’autant plus épais qu’il est chargé.

Cliquez sur un nœud pour ouvrir sa page. La carte se zoome à la molette ou avec les boutons **+** et **−** et se déplace en la faisant glisser ; **« Tout »** montre la carte entière.

## La mettre sur un écran mural

1. Créez un tableau de bord, ajoutez un panneau et, dans **Afficher**, choisissez **« Une carte réseau »**, puis la carte. Donnez au panneau toute la largeur.
2. Sur le tableau de bord, créez un **lien d’affichage** et ouvrez-le sur l’écran.

L’écran n’a besoin d’aucune connexion, montre la carte en lecture seule et toujours ajustée, se rafraîchit toutes les 30 secondes et dit clairement quand il a perdu la connexion. Pour une réunion, **« Plein écran »** sur la page de la carte suffit.

## Qui peut faire quoi

Toute personne qui voit un site voit ses cartes ; les opérateurs les créent et les modifient. La carte d’un client ne montre à chacun que les sites qu’il peut voir. Tout cela est dans l’API publique, sous `/api/v1/maps`.
