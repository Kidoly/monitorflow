---
title: "Comment lancer des scripts sur vos serveurs et automatiser les correctifs"
description: "Gardez vos scripts shell et PowerShell dans MonitorFlow, lancez-les sur un serveur, un service métier ou un site entier, tout de suite ou cette nuit, lisez la sortie de chaque serveur, et lancez-les tout seuls quand un incident s’ouvre."
slug: lancer-des-scripts-et-automatiser
lang: fr
translation: run-scripts-and-automate-fixes
---

# Comment lancer des scripts sur vos serveurs et automatiser les correctifs

Chaque équipe a ses correctifs : vider un cache, faire tourner un journal, redémarrer un service bloqué. MonitorFlow les garde dans sa bibliothèque de scripts, les lance sur les serveurs que vous choisissez, conserve ce que chaque serveur a répondu, et peut les lancer tout seul quand un incident s’ouvre.

## Autoriser les scripts sur les serveurs

Un serveur n’exécute les scripts de la bibliothèque que si son administrateur l’a autorisé à l’installation, sous l’utilisateur de l’agent et jamais en root. L’interface web ne peut pas l’activer : un serveur central compromis ne peut donc pas exécuter de code sur les serveurs qui n’ont pas donné leur accord.

- **Linux** : ajoutez `MF_ALLOW_SCRIPTS=1` à la commande d’installation (cochez « Autoriser les scripts de la bibliothèque » sur la page Ajouter des serveurs), ou relancez l’installeur avec sur un serveur déjà installé. `MF_ALLOW_SCRIPTS=0` le retire.
- **Windows** : installez le MSI avec `ALLOW_SCRIPTS=1`, ou lancez `monitorflow-agent.exe scripts on` en administrateur, puis `Restart-Service MonitorFlowAgent`.

L’onglet Commandes du serveur indique s’il accepte les scripts de la bibliothèque.

## Ajouter un script

Dans **Remédiation › Scripts**, **Nouveau script** :

1. Donnez-lui un nom, comme `clear-app-cache` ; l’IA et les personnes l’appellent ainsi.
2. Choisissez **Shell (Linux)** ou **PowerShell (Windows)**, et le rôle minimum pour le lancer : opérateur, approbateur ou administrateur.
3. Écrivez le script. Ajoutez des paramètres avec un type (texte, nombre entier, oui ou non, choix) et une valeur par défaut ; une valeur arrive dans le script par la variable d’environnement `MF_PARAM_<NOM>`, par exemple `"$MF_PARAM_DAYS"` en shell ou `$env:MF_PARAM_DAYS` en PowerShell. Elle n’est jamais collée dans le texte du script.
4. Cochez **L’IA peut le proposer comme correctif** si la remédiation IA peut s’en servir.

Chaque modification du texte ou des paramètres crée une nouvelle version ; les précédentes restent sur la page du script, et une exécution indique la version qu’elle a lancée.

## Le lancer

Sur la page du script, **Lancer…** : choisissez des serveurs, un site entier ou un service métier, donnez les valeurs, et lancez tout de suite ou à une heure choisie. MonitorFlow classe le script avec les mêmes règles de risque que les commandes IA, ligne par ligne :

- une ligne que les règles interdisent, comme `rm -rf /`, refuse l’exécution ;
- un script de commandes en lecture seule (`df -h`, `free -m`) est en lecture seule ; ce que les règles ne connaissent pas, boucles et variables comprises, est à impact élevé.

L’exécution part aussitôt si votre rôle peut approuver ce risque sur le site de chaque serveur (approbateur pour la lecture seule et l’impact faible, administrateur pour l’impact élevé). Sinon elle attend dans **Remédiation › Approbations**, sous « Scripts en attente d’approbation », jusqu’à 24 heures.

La page de l’exécution montre l’état de chaque serveur, son code de sortie, sa sortie et ses erreurs (les 64 derniers Kio). Un serveur qui refuse les scripts, tourne sur l’autre système ou a été retiré est ignoré, et la page dit pourquoi et comment y autoriser les scripts.

## Le lancer quand quelque chose arrive

Dans **Remédiation › Automatisation**, **Nouvelle règle** : choisissez où elle s’applique (partout, un client ou un site), quand elle se déclenche (un incident s’ouvre, de toute métrique ou d’une seule ; un check échoue ; un serveur rejoint), le script et ses valeurs. La règle lance le script sur le serveur concerné :

- les scripts en lecture seule partent aussitôt, ceux à impact faible si le site laisse les changements de l’IA s’exécuter seuls, les autres attendent une approbation comme une commande IA ;
- au plus une fois par serveur et par délai de carence (60 minutes par défaut), et 50 fois par règle en une heure ;
- « Voir ce qu’elle a fait » liste chaque événement pris, avec son exécution ou la raison pour laquelle elle l’a ignoré.

## Laisser l’IA s’en servir

Les scripts ouverts à l’IA figurent dans ses instructions pendant la remédiation. L’IA en propose un par son nom avec ses valeurs, et MonitorFlow le classe, l’approuve et l’exécute exactement comme toute commande IA, dans les limites de l’incident. Sa sortie revient à l’IA et dans le journal d’audit.

## Par l’API

Tout ce qui précède est dans l’API publique, sous `/api/v1/remediation/scripts`, `/api/v1/remediation/script-runs` et `/api/v1/remediation/automation-rules`, décrits dans la documentation de l’API.
