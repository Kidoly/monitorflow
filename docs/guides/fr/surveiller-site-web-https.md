---
title: "Comment surveiller un site web en HTTPS"
description: "Sachez quand votre site ne répond plus sur le port 443 ou 80, et vérifiez son code de réponse et l'expiration de son certificat avec un script."
slug: surveiller-site-web-https
lang: fr
translation: monitor-website-https
---

# Comment surveiller un site web en HTTPS

Un site peut être inaccessible alors que son serveur semble en forme : serveur web arrêté, règle de pare-feu modifiée, certificat expiré dans la nuit. MonitorFlow vérifie que les ports du site répondent, depuis le serveur web ou une autre machine, et un court script ajoute le code de réponse et les jours restants du certificat.

## Pourquoi c'est important

Les visiteurs ne signalent pas une panne : ils partent. Un certificat expiré est pire : les navigateurs affichent un avertissement en pleine page, souvent un week-end, parce que le renouvellement a échoué sans bruit des semaines plus tôt. Vérifier depuis l'extérieur détecte aussi un changement de réseau ou de pare-feu devant le serveur.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle fait | Alerte par défaut |
| --- | --- | --- |
| « Web server answers (HTTPS) » | Le port 443 accepte les connexions TCP. Métrique `port_443_up` | Critique en cas d'échec, pendant 1 minute |
| « Web server answers (HTTP) » | Le port 80 accepte les connexions TCP. Métrique `port_80_up` | Critique en cas d'échec, pendant 1 minute |
| « Your own script » | Exécute un script que vous installez, lu comme OK, avertissement ou critique | Avertissement s'il signale un problème, pendant 1 minute |

Une vérification de port ouvre une connexion TCP, rien de plus. Elle n'envoie aucune requête et ne lit ni le code de réponse ni le certificat : un site qui répond `502 Bad Gateway` ou dont le certificat a expiré la passe quand même. Le script ci-dessous couvre ces cas.

Par défaut, la vérification se connecte à `127.0.0.1`. Dans « Address », vous pouvez indiquer le nom du site et ajouter la vérification sur un autre serveur, dont l'agent se connecte alors à travers le réseau comme un visiteur. Chaque serveur peut limiter ces destinations avec `port_targets` dans son `agent.toml` ; tout est permis par défaut.

## Mise en place

1. Ouvrez « Fleet » et cliquez sur le serveur qui fera la vérification : le serveur web, ou un autre qui joint le site.
2. Dans l'onglet « Sensors », choisissez « Add a check ».
3. Dans « Find a check », tapez `https` et choisissez « Web server answers (HTTPS) ».
4. « Port » contient `443`. Dans « Address », gardez `127.0.0.1` ou saisissez le nom du site, comme `www.example.com`.
5. Laissez « Raise critical when it fails » coché, choisissez « This server only » sous « Add to », puis cliquez sur « Add check ».

### Code de réponse et certificat, avec un script

L'agent n'exécute que les scripts installés dans `/etc/monitorflow/scripts.d`, sans argument sauf si le serveur l'autorise : écrivez donc l'adresse dans le script. Enregistrez-le sous `check_https.sh` en adaptant les deux premières lignes :

```sh
#!/bin/sh
URL=https://www.example.com/
HOST=www.example.com

code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "$URL")
end=$(timeout 5 openssl s_client -connect "$HOST:443" -servername "$HOST" </dev/null 2>/dev/null \
  | openssl x509 -noout -enddate | cut -d= -f2)
[ -n "$end" ] || { echo "CRITICAL: no certificate from $HOST"; exit 2; }
days=$(( ($(date -d "$end" +%s) - $(date +%s)) / 86400 ))
echo "http_code=$code"
echo "cert_days=$days"
[ "$code" = 200 ] || exit 2
[ "$days" -ge 7 ] || exit 2
[ "$days" -ge 21 ] || exit 1
exit 0
```

Le code de sortie 0 signifie OK, 1 avertissement, 2 critique. curl refuse un certificat invalide, ce qui donne le code `000` et un résultat critique. Chaque ligne `nom=valeur` devient une métrique : `script_check_https_http_code` et `script_check_https_cert_days`. Installez-le et testez-le :

```sh
sudo install -d -o root -g root -m 0755 /etc/monitorflow/scripts.d
sudo install -o root -g root -m 0755 check_https.sh /etc/monitorflow/scripts.d/
sudo monitorflow-agent sensors probe script --param script=check_https.sh
```

Puis « Add a check », choisissez « Your own script », saisissez `check_https.sh` dans « Script », choisissez toutes les 15 minutes sous « How often » et cliquez sur « Add check ». Pour aller plus loin : [Comment surveiller avec votre propre script](surveiller-avec-votre-script.md).

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». « Alert » choisit critique ou avertissement, « For at least » le nombre de minutes que doit durer le problème (`0` alerte dès la première mauvaise valeur, conseillé pour un script lancé toutes les 15 minutes), et « Apply to » ce serveur seul ou tous ceux du site.

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Send test » le vérifie. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: port_443_up is 0 on web-1 (paris)
Warning: script_check_https_status is 1 on web-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Cliquez sur « Acknowledge », puis vérifiez :

1. **D'où vient l'échec :** si seule la vérification depuis un autre serveur échoue, regardez le réseau, le pare-feu ou le DNS entre les deux.
2. **Le serveur web :** voir [nginx](surveiller-nginx.md) ou [Apache](surveiller-apache.md).
3. **Le certificat :** lancez le script à la main pour voir `http_code` et `cert_days` ; vérifiez que le renouvellement s'exécute et que le serveur web a été rechargé ensuite.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
