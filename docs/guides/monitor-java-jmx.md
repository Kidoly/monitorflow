---
title: "How to monitor a Java application (JMX, Tomcat, Kafka)"
description: "Watch a JVM's heap, garbage collection, threads and deadlocks, Tomcat's connectors and a Kafka broker's partitions through Jolokia, and get alerted when it runs out of memory or stops answering, with MonitorFlow's ready-made Java check."
slug: monitor-java-jmx
lang: en
translation: surveiller-java-jmx
---

# How to monitor a Java application (JMX, Tomcat, Kafka)

A Java application can be running and useless at once: its process is there, its port answers, but it spends its time collecting garbage, its threads are all busy or two of them wait on each other forever. The Java virtual machine knows all of it and publishes it as JMX MBeans. MonitorFlow reads them every minute through Jolokia, and tells you when memory runs out, garbage collection takes over, threads deadlock, or a Kafka broker loses replicas.

## Why it matters

A JVM rarely stops cleanly. Its heap fills up, garbage collection runs more and more often, requests slow down, and only at the end comes an `OutOfMemoryError`. A web application whose Tomcat threads are all busy queues requests and looks down to its users while every service check stays green. A Kafka broker can run with partitions under-replicated for days, until a second broker fails and data is lost.

These are numbers the JVM keeps: heap used against its maximum, the time each collector spent, live and deadlocked threads, Tomcat's busy threads against `maxThreads`, Kafka's under-replicated and offline partitions.

## What MonitorFlow checks

### The Java check

The **"Java (JMX)"** entry of the catalogue (an **"Application check"**) reads any JVM that runs the [Jolokia](https://jolokia.org) agent: Java 8 or later, Tomcat, Spring Boot, Kafka, and anything else in Java. Jolokia is a small open source agent that answers JMX reads over HTTP with JSON; the next section shows how to add it. Every minute, the check sends Jolokia one request with every read, and reports:

- whether Jolokia answers with the JVM's memory, how long that takes, and the JVM's version (labels `version` and `vm`);
- heap used, committed and maximum (`-Xmx`), and their ratio; non-heap memory (metaspace, code cache);
- for each garbage collector, collections and time spent since the last check, and the share of time all of them took;
- live, daemon and peak threads, and deadlocked threads;
- the JVM's share of the processor, its open file descriptors against its limit (Unix), and its uptime.

Where they exist, it also reads two groups, both by default:

- **Tomcat** (standalone, or embedded in Spring Boot): for each connector, busy threads against `maxThreads`, requests per second, error responses (status 400 or more) and the average time per request; active sessions of each web application.
- **Kafka** (a broker): under-replicated partitions, partitions under `min.insync.replicas`, offline partitions (on the active controller), whether this broker is the controller, partitions, replicas falling out of sync, messages and bytes in and out per second, and how idle the request handlers are.

A group whose MBeans are not there reports nothing, without error. Metrics are named after the check: `monitorflow_jmx_up`, `monitorflow_jmx_heap_usage_ratio`, `monitorflow_jmx_tomcat_threads_ratio`, and so on (with another check name, that name replaces `jmx`). Per collector, connector or web application, the metric's instance is its name: `G1 Young Generation`, `http-nio-8080`, `localhost/shop`.

With **"Add the recommended alerts"** on, these alerts come with it:

| Metric | Warning | Critical | For |
| --- | --- | --- | --- |
| `monitorflow_jmx_up` (Jolokia answers) | None | Under 1 | 2 minutes |
| `monitorflow_jmx_heap_usage_ratio` | Over 0.9 (90%) | Over 0.95 | 10 minutes |
| `monitorflow_jmx_gc_time_ratio` (time spent collecting) | Over 0.1 (10%) | Over 0.25 | 10 minutes |
| `monitorflow_jmx_threads_deadlocked` | None | Over 0 | At once |
| `monitorflow_jmx_file_descriptors_ratio` | Over 0.8 | Over 0.95 | 5 minutes |
| `monitorflow_jmx_tomcat_threads_ratio` (each connector) | Over 0.9 | None | 5 minutes |
| `monitorflow_jmx_kafka_under_replicated` | Over 0 | None | 5 minutes |
| `monitorflow_jmx_kafka_under_min_isr` | None | Over 0 | 2 minutes |
| `monitorflow_jmx_kafka_offline_partitions` | None | Over 0 | 2 minutes |
| `monitorflow_jmx_kafka_handler_idle_ratio` | Under 0.2 | None | 10 minutes |

An alert whose metric is not there stays quiet: on a JVM without Tomcat or Kafka, those lines never fire.

Heap is measured as the JVM sees it, so it rises and falls with each collection: the 10 minutes keep a busy but healthy JVM from alerting. A heap that stays over 90% for that long is collecting without freeing much.

### Service and port checks

The Java check tells you a JVM is in trouble; a service check tells you its process is gone. Add **"A service is running"** with the systemd unit (`tomcat10`, `kafka`, your application's) or the Windows service, and a port check on the application's own port, as for any service. See [How to monitor anything with your own script](monitor-with-your-own-script.md) for the rest.

## Add Jolokia to the JVM

Jolokia comes as a Java agent, one JAR file loaded when the JVM starts. Download `jolokia-agent-jvm-<version>-javaagent.jar` from [Jolokia's download page](https://jolokia.org/download.html) or Maven Central, and put it where the application's user can read it, for example `/opt/jolokia/jolokia-agent.jar`.

Then add one option to the JVM's command line. Jolokia listens on `127.0.0.1` and port 8778 by default, which is where the check looks:

```
-javaagent:/opt/jolokia/jolokia-agent.jar=port=8778,host=127.0.0.1,user=monitorflow,password=choose-a-long-password,policyLocation=file:///opt/jolokia/jolokia-access.xml
```

Where that option goes depends on the application:

- **Tomcat:** in `bin/setenv.sh` (create it if needed), `CATALINA_OPTS="$CATALINA_OPTS -javaagent:..."`. On Windows, in the service's **Java Options** (`tomcat10w.exe`).
- **Kafka:** in the broker's systemd unit, `Environment="KAFKA_OPTS=-javaagent:..."`. Not in a shell profile: Kafka's command-line tools read `KAFKA_OPTS` too, and would try to open the same port.
- **Spring Boot**, or any `java -jar`: `java -javaagent:... -jar app.jar`, or `JAVA_TOOL_OPTIONS`. For Tomcat's MBeans, also set `server.tomcat.mbeanregistry.enabled=true`.

Restart the application. `curl -u monitorflow:choose-a-long-password http://127.0.0.1:8778/jolokia/read/java.lang:type=Memory/HeapMemoryUsage` should answer with the heap in JSON.

Tomcat can also run Jolokia as a web application (`jolokia-agent-war`) instead: it then answers on Tomcat's own port, so set **"Port"** to `8080` and keep **"Path"** at `/jolokia/`.

### Keep Jolokia read-only

Jolokia can do more than read: write attributes and run any MBean operation, which on some MBeans means stopping the application. The check only reads. Allow only that with a policy file, `/opt/jolokia/jolokia-access.xml`:

```xml
<?xml version="1.0" encoding="utf-8"?>
<restrict>
  <commands>
    <command>read</command>
  </commands>
</restrict>
```

With this policy, deadlocked threads are not counted: finding them is an operation (`findDeadlockedThreads`), and the policy refuses every operation. Everything else is read. To count them as well, add `<command>exec</command>`, knowing it lets whoever holds Jolokia's password run any operation of the JVM; keep Jolokia on `127.0.0.1` with its password if you do.

The `user` and `password` options make Jolokia ask for HTTP Basic authentication. Without them, anyone who reaches the port reads the JVM, so keep `host=127.0.0.1` unless the check runs from elsewhere.

## Set it up

1. Open **Fleet**, click the server, and stay on its **Sensors** tab.
2. Click **"Add a check"** and type `Java` in **"Find a check"** (or `JMX`, `Tomcat`, `Kafka`).
3. Pick **"Java (JMX)"**, the **"Application check"**.
4. Keep **"Address"** at `127.0.0.1` and **"Port"** at `8778` if Jolokia runs on this server with its defaults. Enter `monitorflow` in **"User name"** and its password in **"Password"**, or leave both empty if Jolokia asks for none.
5. **"Also read"** keeps **"Tomcat and Kafka, where they run"**; choose **"The JVM only"** to skip them. Under **"More options"**, **"Path"** is Jolokia's (`/jolokia/`) and **"Encryption"** is **"None (HTTP)"** or **"Required, certificate verified"** for Jolokia over HTTPS.
6. Leave **"Add the recommended alerts"** on, then click **"Add check"**.

The password is kept sealed on the central server, encrypted like its other secrets. It is never shown again, never written to a log, and the API only says whether one is set. If you later change **"Address"** or **"Port"**, enter the password again.

**The password goes over HTTPS only, or to the server itself.** Jolokia's Basic authentication sends it with every request; over plain HTTP from another machine, anyone on the network between could read it. So a check with a password and **"None (HTTP)"** must connect to `127.0.0.1` (or `::1`, `localhost`): from the central server or a relay, give Jolokia a certificate (its `protocol=https` and keystore options) and choose **"Required, certificate verified"**.

**A self-signed certificate.** Jolokia's HTTPS certificate is often one it or its keystore made itself, which no authority signed. From the central server or a relay, such a certificate is trusted by its fingerprint, as a vCenter's is: with **"Required, certificate verified"** chosen, **"Test the certificate"** connects from the central server or relay that runs the check and shows the certificate's SHA-256 fingerprint. Compare it with the one the Java server shows (`keytool -list -v -keystore <keystore>`, or `openssl s_client -connect <address>:8778 </dev/null | openssl x509 -noout -fingerprint -sha256`), then switch on **"I compared it: trust this certificate"**. The fingerprint goes in **"Certificate fingerprint"**, under **"More options"**. The test reads Jolokia, with the password, only once the certificate is trusted; before that, it does a TLS handshake and sends nothing. A check whose agent runs it takes the fingerprint the same way, typed in **"Certificate fingerprint"**: the check's error shows it.

When the certificate changes (a new keystore), the check fails with "the certificate changed: its fingerprint is now ..." and never sends the password to it. On the **Sensors** tab, the check's **"Certificate"** button tests it again; compare the new fingerprint, trust it, then **"Trust this certificate"** saves it. The password is kept.

One check reads one JVM. A server running two (Tomcat and Kafka, say) needs two Jolokia agents on two ports, and two checks with two names.

### Where the check runs

**"Checked from"** chooses who connects:

- **"This server's agent"** (the default): the agent on the server connects to `127.0.0.1`. If its administrator set port targets (`sensors.port_targets` in `agent.toml`), the address and port must be among them.
- **"The central server"**, or **"Relay"** followed by the name of a relay of the site: to read a JVM on a machine without the agent. Make Jolokia listen on the server's address (`host=` in the agent's options) over HTTPS, enter that address in **"Address"**, and allow the central server's or relay's address in the firewall. The central server never connects to link-local addresses (`169.254.x.x`, `fe80::`).

A device, a machine without the agent, has the same check in its catalogue: it always runs from the central server or a relay, against the device's address.

### Through the API

The public API adds the same check: `POST /api/v1/servers/{id}/sensor_configs` with the kind, its parameters, and `default_alerts` to add the recommended alerts. `GET /api/v1/check-templates` lists every metric and alert of each kind.

```json
{
  "kind": "jmx",
  "name": "tomcat",
  "interval_ms": 60000,
  "params": { "host": "127.0.0.1", "port": 8778, "user": "monitorflow", "password": "choose-a-long-password", "groups": "tomcat" },
  "default_alerts": true
}
```

`groups` is `tomcat,kafka` by default, or `tomcat`, `kafka` or `none`; `path` is `/jolokia/`; `tls` is `off` or `verify`; `fingerprint`, with `verify`, is the SHA-256 of a certificate to trust that the machine's roots do not. `POST /api/v1/servers/{id}/jmx/test` shows a certificate as the central server or relay sees it, with the same settings and `probe`, and reads Jolokia only once it is trusted.

## Change the alert

On the **Sensors** tab, the recommended alerts are ordinary alerts, one line each under **"When to alert"**. Open a line's menu and choose **"Edit"**: **"Alert"** sets critical or warning, **"For at least"** the minutes the problem must last (0 alerts at once), and **"Apply to"** this server or every server of the site. A batch job that fills its heap on purpose may want a higher heap threshold, or none: open its menu and change it, or turn it off.

## Where the alert goes

Alerts show at once in **Monitoring › Incidents**. To be told, add a channel under **Settings › Notifications**: email, a webhook, or Slack, Teams or Discord, for everything, one customer or one site. A critical alert nobody acknowledges within 15 minutes also goes to the nearest administrators who set how they are reached on the **On call** page.

## When the alert fires

The message names the check, its value, and the server with its site:

```
Critical: jmx_up is 0 on app-1 (paris)
Critical: jmx_heap_usage_ratio is 0.97 on app-1 (paris)
Critical: jmx_threads_deadlocked is 2 on app-1 (paris)
Warning: jmx_tomcat_threads_ratio http-nio-8080 is 0.95 on app-1 (paris)
Critical: jmx_kafka_under_min_isr is 3 on kafka-2 (paris)
```

An email has that line as subject, after `[MonitorFlow]`, and a link to the alert. A message starting with `Resolved:` follows when it is back to normal.

When `jmx_up` is 0, its `reason` label says why, and its `error` label gives Jolokia's own words:

| `reason` | What to check |
| --- | --- |
| `connect` | Nothing answers at that address and port: the application stopped, Jolokia is not loaded (check the `-javaagent` option), it listens elsewhere (`host`, `port`), or a firewall refuses the connection |
| `timeout` | No answer in time: a firewall that drops packets, or a JVM so busy collecting garbage that it cannot answer, often the worst case |
| `tls` | Jolokia's certificate is not trusted by the machine running the check, or not for that name, or it changed from the fingerprint trusted: the error gives its fingerprint |
| `login` | Wrong user name or password, or Jolokia asks for one and none is set |
| `permission` | Jolokia's policy refuses to read `java.lang:type=Memory`: allow `read` |
| `protocol` | Something else answered on that port, or not at that path (HTTP 404): check **"Path"** |

Click **"Acknowledge"** on the alert's page, then check:

1. **Memory:** a heap at its maximum with a high `gc_time_ratio` needs a larger `-Xmx` or has a leak. `jcmd <pid> GC.heap_info` shows the heap; a heap dump (`jcmd <pid> GC.heap_dump /tmp/heap.hprof`) shows what fills it.
2. **Deadlocks and busy threads:** `jcmd <pid> Thread.print` (or `jstack <pid>`) prints every thread and names the deadlocked ones at the end.
3. **Why it stopped:** `systemctl status` and `journalctl -u <unit> -n 50`, or the application's own log for `OutOfMemoryError`. The server's **Logs** tab shows the journal's warnings and errors too.
4. **Kafka:** `kafka-topics.sh --describe --under-replicated-partitions` (or `--under-min-isr-partitions`) lists the partitions, and the brokers they wait for.

## Not using MonitorFlow yet?

MonitorFlow is free and open source. Install the central server on a Linux machine with one line:

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Then open **Settings › Add servers** in the web interface and paste the command it shows on each server. See [Installing the server](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) and [Installing the agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md).
