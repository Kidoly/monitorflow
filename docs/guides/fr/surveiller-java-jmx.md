---
title: "Comment surveiller une application Java (JMX, Tomcat, Kafka)"
description: "Suivez la heap, le ramasse-miettes, les threads et les interblocages d'une JVM, les connecteurs de Tomcat et les partitions d'un broker Kafka via Jolokia, et soyez alerté quand elle manque de mémoire ou ne répond plus, avec le contrôle Java prêt à l'emploi de MonitorFlow."
slug: surveiller-java-jmx
lang: fr
translation: monitor-java-jmx
---

# Comment surveiller une application Java (JMX, Tomcat, Kafka)

Une application Java peut tourner et ne servir à rien en même temps : son processus est là, son port répond, mais elle passe son temps à ramasser les miettes, tous ses threads sont occupés, ou deux d'entre eux s'attendent pour toujours. La machine virtuelle Java sait tout cela et le publie sous forme de MBeans JMX. MonitorFlow les lit chaque minute via Jolokia, et vous prévient quand la mémoire s'épuise, que le ramasse-miettes prend le dessus, que des threads s'interbloquent ou qu'un broker Kafka perd des réplicas.

## Pourquoi c'est important

Une JVM s'arrête rarement proprement. Sa heap se remplit, le ramasse-miettes passe de plus en plus souvent, les requêtes ralentissent, et seulement à la fin arrive une `OutOfMemoryError`. Une application web dont tous les threads Tomcat sont occupés met les requêtes en file et paraît en panne à ses utilisateurs alors que toutes les vérifications de service restent vertes. Un broker Kafka peut tourner des jours avec des partitions sous-répliquées, jusqu'à ce qu'un second broker tombe et que des données soient perdues.

Ce sont des chiffres que la JVM tient à jour : la heap utilisée par rapport à son maximum, le temps passé par chaque collecteur, les threads vivants et interbloqués, les threads occupés de Tomcat par rapport à `maxThreads`, les partitions sous-répliquées et hors ligne de Kafka.

## Ce que MonitorFlow vérifie

### Le contrôle Java

L'entrée « Java (JMX) » du catalogue (un « Contrôle d’application ») lit toute JVM qui fait tourner l'agent [Jolokia](https://jolokia.org) : Java 8 ou plus récent, Tomcat, Spring Boot, Kafka, et toute autre application Java. Jolokia est un petit agent libre qui répond aux lectures JMX en HTTP avec du JSON ; la section suivante montre comment l'ajouter. Chaque minute, le contrôle envoie à Jolokia une seule requête avec toutes les lectures, et remonte :

- si Jolokia répond avec la mémoire de la JVM, en combien de temps, et la version de la JVM (étiquettes `version` et `vm`) ;
- la heap utilisée, réservée et maximale (`-Xmx`), et leur rapport ; la mémoire hors heap (metaspace, cache de code) ;
- pour chaque ramasse-miettes, les collectes et le temps passé depuis le dernier contrôle, et la part du temps qu'ils ont prise ensemble ;
- les threads vivants, démons et le pic, et les threads interbloqués ;
- la part du processeur prise par la JVM, ses descripteurs de fichiers ouverts par rapport à sa limite (Unix), et sa durée de fonctionnement.

Quand ils existent, il lit aussi deux groupes, tous deux par défaut :

- **Tomcat** (seul, ou intégré à Spring Boot) : pour chaque connecteur, les threads occupés par rapport à `maxThreads`, les requêtes par seconde, les réponses en erreur (statut 400 ou plus) et le temps moyen par requête ; les sessions actives de chaque application web.
- **Kafka** (un broker) : les partitions sous-répliquées, celles sous `min.insync.replicas`, les partitions hors ligne (sur le contrôleur actif), si ce broker est le contrôleur, les partitions, les réplicas qui décrochent, les messages et octets entrants et sortants par seconde, et le temps libre des threads de traitement des requêtes.

Un groupe dont les MBeans ne sont pas là ne remonte rien, sans erreur. Les métriques portent le nom du contrôle : `monitorflow_jmx_up`, `monitorflow_jmx_heap_usage_ratio`, `monitorflow_jmx_tomcat_threads_ratio`, et ainsi de suite (avec un autre nom de contrôle, ce nom remplace `jmx`). Par collecteur, connecteur ou application web, l'instance de la métrique est son nom : `G1 Young Generation`, `http-nio-8080`, `localhost/shop`.

Avec « Ajouter les alertes recommandées » activé, ces alertes l'accompagnent :

| Métrique | Avertissement | Critique | Durée |
| --- | --- | --- | --- |
| `monitorflow_jmx_up` (Jolokia répond) | Aucun | Sous 1 | 2 minutes |
| `monitorflow_jmx_heap_usage_ratio` | Au-dessus de 0,9 (90 %) | Au-dessus de 0,95 | 10 minutes |
| `monitorflow_jmx_gc_time_ratio` (temps passé à collecter) | Au-dessus de 0,1 (10 %) | Au-dessus de 0,25 | 10 minutes |
| `monitorflow_jmx_threads_deadlocked` | Aucun | Au-dessus de 0 | Tout de suite |
| `monitorflow_jmx_file_descriptors_ratio` | Au-dessus de 0,8 | Au-dessus de 0,95 | 5 minutes |
| `monitorflow_jmx_tomcat_threads_ratio` (chaque connecteur) | Au-dessus de 0,9 | Aucun | 5 minutes |
| `monitorflow_jmx_kafka_under_replicated` | Au-dessus de 0 | Aucun | 5 minutes |
| `monitorflow_jmx_kafka_under_min_isr` | Aucun | Au-dessus de 0 | 2 minutes |
| `monitorflow_jmx_kafka_offline_partitions` | Aucun | Au-dessus de 0 | 2 minutes |
| `monitorflow_jmx_kafka_handler_idle_ratio` | Sous 0,2 | Aucun | 10 minutes |

Une alerte dont la métrique n'existe pas reste muette : sur une JVM sans Tomcat ni Kafka, ces lignes ne se déclenchent jamais.

La heap est mesurée comme la JVM la voit : elle monte et descend à chaque collecte. Les 10 minutes évitent qu'une JVM chargée mais saine déclenche une alerte. Une heap qui reste au-dessus de 90 % aussi longtemps collecte sans presque rien libérer.

### Vérifications de service et de port

Le contrôle Java dit qu'une JVM va mal ; une vérification de service dit que son processus a disparu. Ajoutez « A service is running » avec l'unité systemd (`tomcat10`, `kafka`, celle de votre application) ou le service Windows, et une vérification du port de l'application, comme pour tout service. Voir [Comment tout surveiller avec votre propre script](surveiller-avec-votre-script.md) pour le reste.

## Ajouter Jolokia à la JVM

Jolokia se présente comme un agent Java, un seul fichier JAR chargé au démarrage de la JVM. Téléchargez `jolokia-agent-jvm-<version>-javaagent.jar` depuis [la page de téléchargement de Jolokia](https://jolokia.org/download.html) ou Maven Central, et placez-le là où l'utilisateur de l'application peut le lire, par exemple `/opt/jolokia/jolokia-agent.jar`.

Ajoutez ensuite une option à la ligne de commande de la JVM. Jolokia écoute par défaut sur `127.0.0.1` et le port 8778, là où le contrôle le cherche :

```
-javaagent:/opt/jolokia/jolokia-agent.jar=port=8778,host=127.0.0.1,user=monitorflow,password=choisissez-un-long-mot-de-passe,policyLocation=file:///opt/jolokia/jolokia-access.xml
```

L'endroit où placer cette option dépend de l'application :

- **Tomcat** : dans `bin/setenv.sh` (créez-le au besoin), `CATALINA_OPTS="$CATALINA_OPTS -javaagent:..."`. Sous Windows, dans les **Java Options** du service (`tomcat10w.exe`).
- **Kafka** : dans l'unité systemd du broker, `Environment="KAFKA_OPTS=-javaagent:..."`. Pas dans un profil de shell : les outils en ligne de commande de Kafka lisent aussi `KAFKA_OPTS`, et tenteraient d'ouvrir le même port.
- **Spring Boot**, ou tout `java -jar` : `java -javaagent:... -jar app.jar`, ou `JAVA_TOOL_OPTIONS`. Pour les MBeans de Tomcat, réglez aussi `server.tomcat.mbeanregistry.enabled=true`.

Redémarrez l'application. `curl -u monitorflow:choisissez-un-long-mot-de-passe http://127.0.0.1:8778/jolokia/read/java.lang:type=Memory/HeapMemoryUsage` doit répondre avec la heap en JSON.

Tomcat peut aussi faire tourner Jolokia comme application web (`jolokia-agent-war`) : elle répond alors sur le port de Tomcat, donc réglez « Port » sur `8080` et gardez `/jolokia/` dans « Chemin ».

### Garder Jolokia en lecture seule

Jolokia sait faire plus que lire : écrire des attributs et lancer n'importe quelle opération d'un MBean, ce qui, pour certains, veut dire arrêter l'application. Le contrôle ne fait que lire. N'autorisez que cela avec un fichier de politique, `/opt/jolokia/jolokia-access.xml` :

```xml
<?xml version="1.0" encoding="utf-8"?>
<restrict>
  <commands>
    <command>read</command>
  </commands>
</restrict>
```

Avec cette politique, les threads interbloqués ne sont pas comptés : les trouver est une opération (`findDeadlockedThreads`), et la politique refuse toute opération. Tout le reste est lu. Pour les compter aussi, ajoutez `<command>exec</command>`, en sachant que cela permet à qui détient le mot de passe de Jolokia de lancer n'importe quelle opération de la JVM ; gardez alors Jolokia sur `127.0.0.1` avec son mot de passe.

Les options `user` et `password` font demander à Jolokia une authentification HTTP Basic. Sans elles, quiconque atteint le port lit la JVM : gardez `host=127.0.0.1` sauf si le contrôle tourne d'ailleurs.

## Mise en place

1. Ouvrez « Fleet », cliquez sur le serveur et restez sur l'onglet « Sensors ».
2. Cliquez sur « Add a check » et tapez `Java` dans « Find a check » (ou `JMX`, `Tomcat`, `Kafka`).
3. Choisissez « Java (JMX) », le « Contrôle d’application ».
4. Gardez `127.0.0.1` dans « Adresse » et `8778` dans « Port » si Jolokia tourne sur ce serveur avec ses réglages par défaut. Saisissez `monitorflow` dans « Nom d’utilisateur » et son mot de passe dans « Mot de passe », ou laissez les deux vides si Jolokia n'en demande pas.
5. « Lire aussi » garde « Tomcat et Kafka, s’ils tournent » ; choisissez « La JVM seule » pour les ignorer. Sous « More options », « Chemin » est celui de Jolokia (`/jolokia/`) et « Chiffrement » vaut « Aucun (HTTP) », ou « Obligatoire, certificat vérifié » pour Jolokia en HTTPS.
6. Laissez « Ajouter les alertes recommandées » activé, puis cliquez sur « Add check ».

Le mot de passe est gardé scellé sur le serveur central, chiffré comme ses autres secrets. Il n'est jamais réaffiché, jamais écrit dans un journal, et l'API indique seulement s'il est défini. Si vous changez plus tard « Adresse » ou « Port », saisissez de nouveau le mot de passe.

**Le mot de passe ne part qu'en HTTPS, ou vers le serveur lui-même.** L'authentification Basic de Jolokia l'envoie avec chaque requête ; en HTTP simple depuis une autre machine, n'importe qui sur le réseau entre les deux pourrait le lire. Un contrôle avec un mot de passe et « Aucun (HTTP) » doit donc se connecter à `127.0.0.1` (ou `::1`, `localhost`) : depuis le serveur central ou un relais, donnez un certificat à Jolokia (ses options `protocol=https` et de keystore) et choisissez « Obligatoire, certificat vérifié ».

Un contrôle lit une JVM. Un serveur qui en fait tourner deux (Tomcat et Kafka, par exemple) a besoin de deux agents Jolokia sur deux ports, et de deux contrôles avec deux noms.

### Où tourne le contrôle

« Vérifié depuis » choisit qui se connecte :

- « L’agent de ce serveur » (par défaut) : l'agent du serveur se connecte à `127.0.0.1`. Si son administrateur a défini des cibles de ports (`sensors.port_targets` dans `agent.toml`), l'adresse et le port doivent en faire partie.
- « Le serveur central », ou « Relais » suivi du nom d'un relais du site : pour lire une JVM sur une machine sans agent. Faites écouter Jolokia sur l'adresse du serveur (`host=` dans les options de l'agent) en HTTPS, saisissez cette adresse dans « Adresse », et autorisez l'adresse du serveur central ou du relais dans le pare-feu. Le serveur central ne se connecte jamais aux adresses link-local (`169.254.x.x`, `fe80::`).

Un équipement, une machine sans agent, a le même contrôle dans son catalogue : il tourne toujours depuis le serveur central ou un relais, vers l'adresse de l'équipement.

### Par l'API

L'API publique ajoute le même contrôle : `POST /api/v1/servers/{id}/sensor_configs` avec le type, ses paramètres, et `default_alerts` pour ajouter les alertes recommandées. `GET /api/v1/check-templates` liste les métriques et alertes de chaque type.

```json
{
  "kind": "jmx",
  "name": "tomcat",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 8778, "user": "monitorflow", "password": "choisissez-un-long-mot-de-passe", "groups": "tomcat" },
  "default_alerts": true
}
```

`groups` vaut `tomcat,kafka` par défaut, ou `tomcat`, `kafka` ou `none` ; `path` vaut `/jolokia/` ; `tls` vaut `off` ou `verify`.

## Modifier l'alerte

Dans l'onglet « Sensors », les alertes recommandées sont des alertes ordinaires, une ligne chacune sous « When to alert ». Ouvrez le menu d'une ligne et choisissez « Edit » : « Alert » règle critique ou avertissement, « For at least » le nombre de minutes que le problème doit durer (0 : tout de suite), « Apply to » ce serveur ou tous ceux du site. Un traitement par lots qui remplit sa heap exprès peut mériter un seuil plus haut, ou aucun : ouvrez son menu et modifiez-le, ou désactivez-le.

## Où part l'alerte

Les alertes s'affichent aussitôt dans « Monitoring › Incidents ». Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site. Une alerte critique que personne n'acquitte en 15 minutes part aussi aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

Le message indique la vérification, sa valeur, et le serveur avec son site :

```
Critical: jmx_up is 0 on app-1 (paris)
Critical: jmx_heap_usage_ratio is 0.97 on app-1 (paris)
Critical: jmx_threads_deadlocked is 2 on app-1 (paris)
Warning: jmx_tomcat_threads_ratio http-nio-8080 is 0.95 on app-1 (paris)
Critical: jmx_kafka_under_min_isr is 3 on kafka-2 (paris)
```

Un e-mail reprend cette ligne en objet, après `[MonitorFlow]`, avec un lien vers l'alerte. Un message commençant par `Resolved:` suit quand tout est revenu à la normale.

Quand `jmx_up` vaut 0, son étiquette `reason` dit pourquoi, et son étiquette `error` donne les mots de Jolokia :

| `reason` | Que vérifier |
| --- | --- |
| `connect` | Rien ne répond à cette adresse et ce port : l'application est arrêtée, Jolokia n'est pas chargé (vérifiez l'option `-javaagent`), il écoute ailleurs (`host`, `port`), ou un pare-feu refuse la connexion |
| `timeout` | Pas de réponse à temps : un pare-feu qui jette les paquets, ou une JVM si occupée à ramasser les miettes qu'elle ne peut plus répondre, souvent le pire des cas |
| `tls` | Le certificat de Jolokia n'est pas reconnu par la machine qui fait le contrôle, ou pas pour ce nom |
| `login` | Mauvais nom d'utilisateur ou mot de passe, ou Jolokia en demande un et aucun n'est saisi |
| `permission` | La politique de Jolokia refuse de lire `java.lang:type=Memory` : autorisez `read` |
| `protocol` | Autre chose a répondu sur ce port, ou rien à ce chemin (HTTP 404) : vérifiez « Chemin » |

Cliquez sur « Acknowledge » sur la page de l'alerte, puis vérifiez :

1. **La mémoire :** une heap à son maximum avec un `gc_time_ratio` élevé a besoin d'un `-Xmx` plus grand, ou fuit. `jcmd <pid> GC.heap_info` montre la heap ; un vidage (`jcmd <pid> GC.heap_dump /tmp/heap.hprof`) montre ce qui la remplit.
2. **Les interblocages et threads occupés :** `jcmd <pid> Thread.print` (ou `jstack <pid>`) affiche chaque thread et nomme les threads interbloqués à la fin.
3. **La cause de l'arrêt :** `systemctl status` et `journalctl -u <unité> -n 50`, ou le journal de l'application pour une `OutOfMemoryError`. L'onglet « Logs » du serveur affiche aussi les avertissements et erreurs du journal.
4. **Kafka :** `kafka-topics.sh --describe --under-replicated-partitions` (ou `--under-min-isr-partitions`) liste les partitions, et les brokers qu'elles attendent.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
