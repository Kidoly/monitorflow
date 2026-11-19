---
title: "Comment surveiller VMware vSphere et Hyper-V"
description: "Surveillez les hôtes, clusters, machines virtuelles et banques de données d'un vCenter, d'un hôte ESXi ou d'un hôte Hyper-V, soyez alerté des hôtes déconnectés, des banques pleines et des vieux instantanés, et reliez chaque machine virtuelle à son serveur."
slug: surveiller-vmware-hyper-v
lang: fr
translation: monitor-vmware-hyper-v
---

# Comment surveiller VMware vSphere et Hyper-V

Un hyperviseur porte beaucoup de serveurs à la fois. Quand un hôte perd son vCenter, qu'une banque de données se remplit ou qu'un instantané reste oublié des semaines, toutes ses machines virtuelles sont en danger, y compris celles sans l'agent MonitorFlow. MonitorFlow lit vCenter et ESXi par leur API, et Hyper-V par l'agent de l'hôte Windows, sans agent dans chaque machine virtuelle.

## Pourquoi c'est important

- **Une banque de données pleine** arrête d'un coup toutes les machines virtuelles qui y écrivent.
- **Un hôte déconnecté** garde ses machines virtuelles en marche, mais personne ne peut plus les gérer ni les déplacer, et la haute disponibilité peut ne plus les couvrir.
- **Un vieil instantané** grossit à chaque écriture jusqu'à remplir sa banque, et ralentit la machine.
- **Une réplication arrêtée** (Hyper-V) rend le plan de reprise inutilisable, sans que personne le voie avant d'en avoir besoin.

## VMware : ajouter le vCenter ou l'hôte ESXi

### 1. Créer un compte en lecture seule

MonitorFlow ne fait que lire. Un compte avec le rôle intégré **Lecture seule** suffit.

- **vCenter** : dans le vSphere Client, *Administration › Single Sign On › Utilisateurs et groupes*, ajoutez un utilisateur comme `monitor@vsphere.local`. Puis *Administration › Contrôle d'accès › Autorisations globales*, ajoutez-le avec le rôle **Lecture seule** et cochez **Propager aux enfants**.
- **Un hôte ESXi seul** : dans le client de l'hôte, *Hôte › Gérer › Sécurité et utilisateurs › Utilisateurs*, ajoutez un utilisateur. Puis *Hôte › Actions › Autorisations*, donnez-lui le rôle **Lecture seule**.

Une autorisation manquante cache seulement des objets de l'inventaire, sans jamais donner d'erreur.

### 2. L'ajouter comme équipement

Dans le parc, **Ajouter un équipement** : son nom et son adresse, puis cochez **C'est un vCenter ou un hôte ESXi** et saisissez l'utilisateur et le mot de passe. **Tester la connexion** : MonitorFlow se connecte depuis le relais du site (ou le serveur central) et montre ce qui répond et son certificat.

La plupart des vCenter et des hôtes ESXi utilisent un certificat de leur propre autorité. MonitorFlow montre alors son **empreinte SHA-256** et n'envoie pas le mot de passe tant que vous ne lui faites pas confiance. Comparez-la avec celle qu'affiche le vCenter (ouvrez son adresse dans un navigateur et regardez les détails du certificat), ou lisez-la depuis une machine de confiance :

```
openssl s_client -connect vcenter.example.lan:443 </dev/null 2>/dev/null | openssl x509 -noout -fingerprint -sha256
```

Cochez **J'ai comparé : faire confiance à ce certificat**, testez à nouveau : le résultat de la connexion indique ce que le compte voit. **Ajoutez l'équipement**.

Quand le certificat du vCenter change plus tard, sa page le dit avec la nouvelle empreinte : ouvrez les **Détails** de l'équipement, **vCenter › Modifier…**, testez et faites confiance au nouveau.

## Ce que MonitorFlow vérifie

Toutes les 5 minutes, une requête lit tout l'inventaire. La page de l'équipement liste :

| Quoi | Affiché |
| --- | --- |
| **Hôtes** | Connecté, déconnecté, ne répond pas ou en maintenance ; processeur et mémoire utilisés ; état général et matériel ; alarmes |
| **Clusters** | Hôtes en service sur le total ; alarmes |
| **Machines virtuelles** | En marche ou arrêtée ; processeur, mémoire et stockage ; instantanés et l'âge du plus ancien ; alarmes ; le serveur qu'elle est |
| **Banques de données** | Espace libre et capacité ; accessible ou non ; alarmes |

Les alertes qui viennent avec, modifiables sur la page de l'équipement comme tout seuil :

| Alerte | Par défaut |
| --- | --- |
| L'API ne répond plus | Critique après 2 minutes |
| Hôte déconnecté | Critique après 5 minutes |
| Banque de données pleine | Avertissement sous 10 % libre, critique sous 5 %, pendant 10 minutes |
| Instantané de plus de 7 jours | Avertissement, par machine virtuelle |
| Alarme rouge sur un hôte ou une banque | Avertissement |
| Capteur matériel pas au vert | Avertissement en jaune, critique en rouge, pendant 5 minutes |

## Hyper-V : le surveiller avec l'agent

Installez l'agent MonitorFlow sur le serveur Windows qui fait tourner Hyper-V. Hyper-V n'a pas de rôle en lecture seule : ses commandes de gestion ne répondent qu'aux administrateurs et aux membres des **Administrateurs Hyper-V**. Ajoutez le compte de l'agent à ce groupe, dans PowerShell en administrateur, puis redémarrez l'agent :

```
Add-LocalGroupMember -Group "Hyper-V Administrators" -Member "NT SERVICE\MonitorFlowAgent"
Restart-Service MonitorFlowAgent
```

Sur un Windows en français, le groupe s'appelle « Administrateurs Hyper-V ». L'agent ne lance qu'un script fixe qui lit ; il ne prend aucune commande du serveur. Sur la page du serveur, **Surveiller Hyper-V** ajoute le contrôle toutes les 5 minutes. Chaque machine virtuelle montre son état, son processeur, sa mémoire, son stockage, ses points de contrôle (les instantanés de Hyper-V) et l'état de sa réplication, avec ces alertes :

| Alerte | Par défaut |
| --- | --- |
| Hyper-V ne peut pas être lu | Avertissement après 15 minutes, avec la raison, comme le groupe ci-dessus |
| Point de contrôle de plus de 7 jours | Avertissement |
| État de la réplication | Avertissement, critique quand Hyper-V le dit critique, pendant 10 minutes |

## Les machines virtuelles et les serveurs déjà surveillés

MonitorFlow relie chaque machine virtuelle au serveur ou à l'équipement du même client qu'elle est : par son identifiant matériel (que l'agent remonte), sa carte réseau, son adresse, ou son nom quand rien d'autre ne correspond. La ligne de la machine virtuelle nomme son serveur et la façon dont elle a été associée, et la page du serveur dit « Machine virtuelle web-01 sur l'hôte esx-02 de vcenter-1 ». Quand une association est fausse, **Pas celle-ci** la retire pour de bon ; **Associer automatiquement** la rend à l'association automatique.

Une machine virtuelle sans l'agent propose **Installer l'agent** : un serveur en attente de son agent est ajouté, au nom de la machine virtuelle, et sa page s'ouvre avec la commande d'installation.

## Quand une alerte se déclenche

- **Banque de données pleine** : cherchez d'abord les vieux instantanés de ses machines virtuelles, puis les disques fins qui ont grossi ; déplacez une machine virtuelle vers une autre banque.
- **Hôte déconnecté** : vérifiez le réseau de gestion de l'hôte et ses services `hostd` et `vpxa` ; ses machines virtuelles tournent toujours.
- **Vieil instantané** : demandez qui l'a pris avant de le supprimer ; un outil de sauvegarde peut l'avoir laissé après un échec.
- **Hyper-V ne peut pas être lu** : le message dit pourquoi, le plus souvent l'appartenance au groupe ci-dessus.

## Venir d'un autre outil

L'import lit les hôtes Zabbix avec le modèle VMware (l'URL et l'utilisateur du vCenter ; saisissez le mot de passe ensuite) et les capteurs VMware et Hyper-V de PRTG. Voir [venir d'un autre outil](venir-d-un-autre-outil.md) et [migrer depuis PRTG](migrer-depuis-prtg.md).
