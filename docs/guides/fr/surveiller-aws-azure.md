---
title: "Comment surveiller les machines virtuelles AWS, Azure et Google Cloud"
description: "Surveillez les instances EC2 d'un compte AWS, les machines virtuelles d'un abonnement Azure et les instances Compute Engine d'un projet Google Cloud : leur état, leurs contrôles d'état, leur processeur, leur réseau et leurs disques, avec des identifiants en lecture seule, et reliez chacune à son serveur."
slug: surveiller-aws-azure
lang: fr
translation: monitor-aws-azure
---

# Comment surveiller les machines virtuelles AWS, Azure et Google Cloud

Un compte cloud contient des machines où personne n'a installé l'agent, et des machines dont l'hôte tombe en panne sans qu'elles le sachent. MonitorFlow lit un compte AWS, un abonnement Azure ou un projet Google Cloud par son API, toutes les 5 minutes depuis le serveur central, avec des identifiants qui ne peuvent que lire. Chaque machine virtuelle montre son état, ses contrôles d'état et ses métriques de base, et elle est reliée au serveur que vous surveillez déjà quand c'en est un.

## Pourquoi c'est important

- **Un contrôle d'état en échec** veut dire que le fournisseur voit la machine ou son hôte en panne (réseau perdu, défaut matériel, volume plein ou dégradé), souvent avant que la machine elle-même ne s'en aperçoive.
- **Une machine arrêtée** qui devrait tourner, ou une machine oubliée qui tourne et coûte, se voit dans une seule liste par compte.
- **Les machines sans l'agent** sont listées aussi, avec un bouton pour l'installer.

## AWS : créer une clé d'accès en lecture seule

MonitorFlow appelle trois actions, dont aucune ne change quoi que ce soit. Créez-lui un utilisateur IAM avec seulement cette stratégie :

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Sid": "MonitorFlowReadOnly",
      "Effect": "Allow",
      "Action": [
        "ec2:DescribeInstances",
        "ec2:DescribeInstanceStatus",
        "cloudwatch:GetMetricData"
      ],
      "Resource": "*"
    }
  ]
}
```

Ces actions ne portent sur aucune ressource : `"Resource": "*"` est donc le plus étroit qu'AWS permette. Ensuite, sur l'utilisateur, **Informations d'identification de sécurité › Créer une clé d'accès** donne l'ID de clé d'accès (`AKIA…`) et sa clé d'accès secrète. Sans `cloudwatch:GetMetricData`, les machines sont toujours listées et le compte indique que les métriques ne sont pas lisibles.

CloudWatch facture `GetMetricData` par métrique demandée : MonitorFlow en demande cinq par instance en marche toutes les 5 minutes (processeur, réseau entrant et sortant, lectures et écritures disque), soit environ 43 200 par mois et par instance. Les contrôles d'état viennent d'EC2 et ne coûtent rien.

## Azure : créer un principal de service avec le rôle Lecteur

Le rôle intégré **Lecteur** (Reader) sur l'abonnement suffit. Avec Azure CLI :

```
az ad sp create-for-rbac --name monitorflow-reader --role Reader --scopes /subscriptions/<subscription-id>
```

Il affiche `appId` (l'ID d'application, ou client), `password` (le secret client) et `tenant`. Pour donner moins que Lecteur, créez un rôle personnalisé avec seulement ces actions et attribuez-le sur l'abonnement à la place :

```
Microsoft.Compute/virtualMachines/read
Microsoft.Compute/virtualMachines/instanceView/read
Microsoft.Network/networkInterfaces/read
Microsoft.Insights/metrics/read
```

Sans `networkInterfaces/read`, les machines sont listées sans leurs cartes réseau ni leurs adresses, et moins d'entre elles sont reliées à leur serveur.

## Google Cloud : créer un compte de service avec une clé JSON

Les rôles **Lecteur de Compute** (Compute Viewer) et **Lecteur Monitoring** (Monitoring Viewer) sur le projet suffisent. Avec la CLI gcloud :

```
gcloud iam service-accounts create monitorflow-reader --project <project-id>
gcloud projects add-iam-policy-binding <project-id> \
  --member serviceAccount:monitorflow-reader@<project-id>.iam.gserviceaccount.com --role roles/compute.viewer
gcloud projects add-iam-policy-binding <project-id> \
  --member serviceAccount:monitorflow-reader@<project-id>.iam.gserviceaccount.com --role roles/monitoring.viewer
gcloud iam service-accounts keys create monitorflow-key.json \
  --iam-account monitorflow-reader@<project-id>.iam.gserviceaccount.com
```

La dernière commande écrit le fichier de clé. Il contient une clé privée : donnez-le à MonitorFlow, puis supprimez-le. Pour donner moins que ces rôles, créez un rôle personnalisé avec seulement `compute.instances.list` et `monitoring.timeSeries.list`. Si votre organisation interdit les clés de compte de service (la règle `iam.disableServiceAccountKeyCreation`), un administrateur doit les autoriser pour ce projet. Le même compte de service peut lire d'autres projets dès qu'il y a les rôles : ajoutez un équipement par projet.

MonitorFlow demande à Cloud Monitoring cinq listes par projet toutes les 5 minutes, quel que soit le nombre de machines, soit environ 43 200 appels par mois et par projet. Consultez la tarification de Cloud Monitoring de Google pour les lectures au-delà du quota gratuit.

## Ajouter le compte

Dans le parc, **Ajouter un équipement** : un nom comme *AWS production*, puis cochez **C'est un compte cloud**. Choisissez le fournisseur et remplissez ce qu'il demande :

| Fournisseur | Champs |
| --- | --- |
| AWS | ID de clé d'accès, clé d'accès secrète, régions (comme `eu-west-3, us-east-1`) |
| Azure | ID de l'annuaire (locataire), ID d'application (client), secret client, ID d'abonnement |
| Google Cloud | Le fichier de clé du compte de service, ID du projet (celui de la clé est rempli) |

**Tester les identifiants** : le serveur central se connecte une fois et dit combien de machines il voit, combien tournent, et si les métriques sont lisibles. **Ajouter l'équipement**. Il n'y a pas d'adresse à taper ni de ping : la page du compte montre ses machines dans les 5 minutes.

Le secret est enregistré scellé, n'est plus jamais affiché et n'est jamais écrit dans les journaux ni dans le journal d'audit. Dans les **Détails** de l'équipement, **Compte cloud › Modifier…** change les régions, le projet ou le secret ; une nouvelle clé d'accès, un autre locataire ou un autre compte de service demande son propre secret. Les relais ne reçoivent jamais d'identifiants cloud : le compte est toujours lu par le serveur central.

## Ce que MonitorFlow vérifie

| Affiché | AWS | Azure | Google Cloud |
| --- | --- | --- | --- |
| État | En marche, arrêtée, en attente | En marche, arrêtée, désallouée | En marche, arrêtée (terminated), suspendue |
| Zone et type | Zone de disponibilité, type d'instance | Région, taille | Zone, type de machine |
| Processeur | CloudWatch `CPUUtilization` | Azure Monitor `Percentage CPU` | Cloud Monitoring `instance/cpu/utilization` |
| Réseau | `NetworkIn`, `NetworkOut` | `Network In Total`, `Network Out Total` | `instance/network/received_bytes_count`, `sent_bytes_count` |
| Disques | `EBSReadBytes`, `EBSWriteBytes` | `Disk Read Bytes`, `Disk Write Bytes` | `instance/disk/read_bytes_count`, `write_bytes_count` |
| Contrôles d'état | Contrôles du système, de l'instance et des volumes attachés | Disponibilité de la machine vue par Azure (`VmAvailabilityMetric`) | Aucun : Compute Engine n'en a pas |

Le réseau et les disques sont affichés par seconde, en moyenne sur 5 minutes. La surveillance de base d'EC2 a un point toutes les 5 minutes : la valeur la plus récente peut donc dater de 10 minutes. Les machines arrêtées n'ont ni métriques ni contrôles d'état.

Les alertes fournies, modifiables sur la page de l'équipement comme tout seuil :

| Alerte | Par défaut |
| --- | --- |
| Le compte n'est pas lisible (identifiants refusés, droit manquant, API qui ne répond pas) | Critique après 10 minutes, avec le message du fournisseur |
| Un contrôle d'état échoue (AWS et Azure) | Critique après 10 minutes, par machine |

## Les machines et les serveurs déjà surveillés

MonitorFlow relie chaque machine au serveur ou à l'équipement du même client qu'elle est, comme pour VMware : sur Azure par son identifiant matériel (l'ID de VM est celui que l'agent remonte), sur AWS et Google Cloud par l'adresse MAC d'une carte réseau (sur Google Cloud, `42:01` suivi de l'adresse interne), puis par une adresse ou le nom d'hôte. La ligne de la machine nomme son serveur et la façon dont il a été trouvé ; **Pas celle-ci** et **Associer automatiquement** le corrigent. Une machine sans l'agent a **Installer l'agent**.

## Quand une alerte se déclenche

- **Un contrôle d'état échoue** sur AWS : un contrôle du système désigne l'hôte ; arrêtez puis démarrez l'instance (sans simple redémarrage) pour la déplacer sur un autre. Un contrôle de l'instance désigne la machine elle-même : regardez la sortie de sa console pour un disque plein ou un réglage réseau. Sur Azure, regardez l'**État des ressources** de la machine.
- **Le compte n'est pas lisible** : le message dit ce qu'AWS, Azure ou Google Cloud a refusé : une clé d'accès supprimée ou renouvelée, un secret client expiré (ils durent deux ans au plus), une clé de compte de service supprimée ou désactivée, ou un droit retiré.
