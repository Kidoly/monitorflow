---
title: "Passer de Zabbix ou Prometheus à MonitorFlow"
description: "Importez vos hôtes, vérifications, seuils, maintenances et canaux de notification depuis Zabbix ou Prometheus, prévisualisez ce qui est repris, puis installez l’agent."
slug: venir-d-un-autre-outil
lang: fr
translation: move-from-another-tool
---

# Passer de Zabbix ou Prometheus à MonitorFlow

Changer d’outil de supervision veut souvent dire tout ressaisir : des centaines d’hôtes, leurs vérifications, des seuils ajustés au fil des années, les maintenances et qui est prévenu. MonitorFlow lit plutôt la configuration de votre outil actuel, vous montre ce qu’il sait faire de la même façon, ce qu’il fait autrement et ce qu’il ne sait pas faire, et ne crée rien avant votre confirmation. Vos serveurs n’ont ensuite besoin que de l’agent MonitorFlow, et l’ancien outil peut continuer de tourner jusqu’à ce que vous l’arrêtiez.

## Ce qui est repris

| Dans votre outil actuel | Dans MonitorFlow |
| --- | --- |
| Hôtes, cibles | Serveurs en attente de leur agent, avec leur nom, leurs adresses et leur système |
| Éléments, jobs de collecte, sondes | Vérifications : services, ports TCP, motifs dans les journaux ; processeur, mémoire et disques sont déjà mesurés sur chaque serveur |
| Déclencheurs, règles d’alerte | Alertes avec niveaux d’avertissement et critique, durée avant alerte et marge de retour |
| Maintenances, silences | Silences des serveurs concernés |
| Types de média et médias des utilisateurs, destinataires | Canaux de notification e-mail, webhook, Slack, Teams et Discord |

Chaque ligne de l’aperçu est dans l’une de trois listes :

- **Pareil dans MonitorFlow** : la correspondance est exacte.
- **Approché** : MonitorFlow fait quelque chose de proche, et la ligne dit en quoi il diffère. Un scénario web devient par exemple une vérification que le port du site répond, et une maintenance récurrente devient le silence de sa prochaine fenêtre.
- **Ne peut pas être repris** : la ligne dit pourquoi, avec l’expression ou la clé d’origine, pour que vous décidiez quoi en faire.

## Importer depuis Zabbix

Il vous faut un compte administrateur Zabbix. Zabbix 6.0, 6.4 et 7.0 sont pris en charge.

**Par son API** (recommandé : elle inclut les maintenances et qui est prévenu) :

1. Dans Zabbix, allez dans **Utilisateurs › Jetons API** et créez un jeton pour un utilisateur Super admin.
2. Dans MonitorFlow, allez dans **Paramètres › Venir d’un autre outil**, choisissez **Zabbix**, puis **Se connecter à son API**.
3. Saisissez l’adresse de l’interface web de Zabbix (comme `https://zabbix.example.com`) et le jeton, puis **Lire et prévisualiser**.

MonitorFlow ne fait que lire dans Zabbix et ne garde pas le jeton.

**Par des fichiers d’export** : dans Zabbix, sélectionnez vos hôtes sous **Collecte de données › Hôtes** et choisissez **Exporter** (YAML, XML ou JSON). Exportez de même les modèles qu’ils utilisent et vos types de média, puis envoyez tous les fichiers ensemble. Les exports ne contiennent ni maintenances ni médias des utilisateurs : les silences et les destinataires des canaux e-mail ne viennent que par l’API.

Ce qui correspond à quoi :

- Les éléments processeur, mémoire, système de fichiers et activité disque sont déjà mesurés par chaque serveur MonitorFlow. Leurs déclencheurs deviennent des alertes aux mêmes niveaux, macros résolues par hôte.
- `net.tcp.service` et `net.tcp.port` deviennent des vérifications de port TCP ; `systemd.unit.*` et les `service.*` Windows des vérifications de service ; `log[]` et `logrt[]` des vérifications de journal.
- Les scénarios web et `web.page.*` deviennent des vérifications du port du site ; `proc.num` une vérification du service systemd de ce nom.
- Les déclencheurs de disponibilité de l’agent et `agent.ping` correspondent à l’alerte hors ligne de MonitorFlow, présente sur chaque serveur.
- Un hôte lié à un modèle PostgreSQL, MySQL, MariaDB, Redis ou Docker de l’agent Zabbix ou de l’agent 2 reçoit le contrôle prêt à l’emploi de MonitorFlow de ce type, avec ses alertes par défaut, connecté là où le disent les macros du modèle ; les éléments et déclencheurs du modèle ne sont pas listés en plus. Les mots de passe ne sont pas importés : saisissez-les sur le contrôle. Les modèles ODBC ne peuvent pas être repris, sauf « MSSQL by ODBC », qui devient le contrôle SQL Server sur `{$MSSQL.PORT}`, comme « MSSQL by Zabbix agent 2 » sur `{$MSSQL.URI}`.
- Un hôte lié à un modèle JMX (« Generic Java JMX », « Apache Tomcat by JMX », « Apache Kafka by JMX ») reçoit à la place le contrôle Java de MonitorFlow, avec ses alertes par défaut. Il lit la JVM via l’agent Jolokia sur son port 8778, et non via la passerelle Java de Zabbix : ajoutez Jolokia à la JVM comme le montre [Comment surveiller une application Java](surveiller-java-jmx.md).
- Le trafic des interfaces réseau, le SNMP, `system.run` et les UserParameters ne peuvent pas encore être repris.

## Importer depuis Prometheus

**Par son API** : choisissez **Prometheus**, puis **Se connecter à son API**, et saisissez l’adresse de Prometheus (comme `http://prometheus.example.com:9090`) et, si vous voulez, celle d’Alertmanager. Avec un Prometheus en marche, MonitorFlow voit les cibles résolues par la découverte par fichiers et les vrais noms d’hôte que donne node exporter. L’API d’Alertmanager masque les secrets : les destinataires webhook et de messagerie viennent donc plutôt des fichiers.

**Par des fichiers** : envoyez ensemble `prometheus.yml`, les fichiers de règles, les fichiers de découverte et `alertmanager.yml`.

Ce qui correspond à quoi :

- Les cibles node exporter deviennent des serveurs. Les sondes TCP d’un blackbox exporter deviennent des vérifications de port, ses sondes HTTP des vérifications du port du site, sur le serveur qui fait tourner l’exporter.
- Les règles node habituelles (cible injoignable, processeur, charge, mémoire, espace disque, latence disque, unités systemd) deviennent des alertes avec leurs niveaux et leur durée `for:`, et `probe_success == 0` l’alerte de chaque sonde.
- Les cibles postgres_exporter (port 9187), mysqld_exporter (9104) et redis_exporter (9121) deviennent le contrôle PostgreSQL, MySQL ou Redis du serveur de l’exporter, au port par défaut de la base, un job nommé d’après SQL Server (`mssql`, `sqlserver`) le contrôle SQL Server de son serveur, et un job `cadvisor` le contrôle Docker de son serveur, chacun avec ses alertes par défaut. Les autres métriques de l’exporter ne sont pas reprises ; saisissez le mot de passe de la base sur le contrôle.
- Un job dont le nom contient `jmx` (l’exporter JMX) devient le contrôle Java de son serveur, qui lit Jolokia sur le port 8778 de la machine de l’exporter, avec ses alertes par défaut. Ajoutez Jolokia à la JVM ; les autres métriques de l’exporter ne sont pas reprises.
- Toute autre expression est listée comme ne pouvant pas être reprise, avec l’expression.
- Les destinataires deviennent des canaux : e-mail, webhook, Slack, Microsoft Teams et Discord. Les routes d’Alertmanager ne sont pas reprises : dans MonitorFlow, un canal couvre tout ou un site.
- Les silences actifs deviennent des silences des serveurs que leurs filtres désignent.

## Prévisualiser, confirmer, annuler

1. Lisez les trois listes. Décochez ce que vous ne voulez pas : ce qui en dépend (les vérifications d’un serveur décoché) est laissé de côté aussi.
2. Choisissez **Créer**. Tout est créé en une fois et inscrit au journal d’audit.
3. Importer de nouveau la même configuration ne crée rien deux fois : les lignes que MonitorFlow a déjà affichent **Déjà là**.
4. Vous changez d’avis ? Ouvrez l’import sous **Imports précédents** et choisissez **Annuler cet import**. Il supprime ce que l’import a créé, sauf les serveurs dont l’agent est déjà arrivé, qui gardent leurs vérifications.

Tout cela existe aussi dans l’API publique, sous `/api/v1/imports` : lister les sources, lire des fichiers ou se connecter, lire l’aperçu, confirmer avec les éléments à laisser de côté, et annuler.

## Installer les agents

Les serveurs importés affichent **En attente de l’agent** dans le parc et ne déclenchent pas d’alerte hors ligne. Sur la page d’un serveur, **Créer une commande d’installation** donne une commande prête à coller pour ce serveur. Vous pouvez aussi utiliser une clé d’installation du site depuis **Paramètres › Ajouter des serveurs** : un agent qui s’inscrit avec le même nom d’hôte, ou sa forme courte, reprend le serveur importé avec toutes ses vérifications et alertes.

Laissez l’ancien outil tourner pendant que les agents arrivent. Les deux vous préviennent pendant ce temps ; arrêtez l’ancien quand chaque serveur remonte dans MonitorFlow.
