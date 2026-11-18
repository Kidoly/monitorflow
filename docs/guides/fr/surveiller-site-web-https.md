---
title: "Comment surveiller un site web en HTTPS"
description: "Sachez quand votre site ne répond plus comme il le devrait : son code de réponse, le texte de la page, son temps de réponse, ses redirections et l'expiration de son certificat."
slug: surveiller-site-web-https
lang: fr
translation: monitor-website-https
---

# Comment surveiller un site web en HTTPS

Un site peut être inaccessible alors que son serveur semble en forme : l'application répond `502 Bad Gateway`, un déploiement a laissé une page d'erreur, une redirection tourne en boucle, ou le certificat a expiré dans la nuit. MonitorFlow demande la page comme le ferait un visiteur et vous dit ce qui ne va pas, depuis le serveur web lui-même ou depuis une autre machine.

## Pourquoi c'est important

Les visiteurs ne signalent pas un site en panne : ils partent. Un certificat expiré est pire : les navigateurs affichent un avertissement sur toute la page, souvent un week-end, parce que le renouvellement a échoué en silence des semaines plus tôt. Vérifier depuis l'extérieur détecte aussi un changement de réseau ou de pare-feu devant le serveur.

## Ce que MonitorFlow vérifie

| Entrée du catalogue | Ce qu'elle fait | Alertes par défaut |
| --- | --- | --- |
| **« Un site web répond »** | Demande la page : son code de statut, son temps de réponse, ses redirections et, en https, son certificat | Critique en cas d'échec pendant 2 minutes |
| **« Une page affiche le bon contenu »** | La même chose, avec un texte que la page doit contenir (un nom de produit) ou ne pas contenir (un message d'erreur) | Les mêmes |
| **« Un certificat n'expire pas »** | La même chose, pour une adresse `https://` | Les mêmes |
| **« Une API répond »** | La même chose, avec le code de statut attendu, des en-têtes et un mot de passe ou un jeton | Les mêmes |

Chaque contrôle de site web vient avec ces alertes, modifiables comme les autres :

- critique quand la page ne répond pas comme prévu pendant 2 minutes : pas de réponse, un code de statut inattendu, le texte absent, trop de redirections ;
- critique quand le certificat n'est pas valide pendant 5 minutes : expiré, autosigné, signé par une autorité que la machine qui contrôle ne reconnaît pas, ou pour un autre nom ;
- avertissement quand il reste moins de 14 jours au certificat, critique sous 7 ;
- avertissement quand la page met plus de 3 secondes pendant 5 minutes.

Un certificat expiré ne masque pas le reste : la page est tout de même lue, vous voyez donc à la fois son code de statut et ce qui ne va pas avec le certificat.

Le contrôle tourne sur l'agent du serveur auquel vous l'ajoutez, sauf si **« Vérifié depuis »** désigne le serveur central ou un relais du site. Ajoutez-le au serveur web pour vérifier le site tel qu'il le sert, à un autre serveur pour le vérifier à travers le réseau, ou faites-le vérifier de l'extérieur par le serveur central ou un relais, comme un visiteur. Un équipement, qui n'a pas d'agent, reçoit aussi des contrôles de site web, lancés depuis le serveur central ou un relais. Chaque serveur peut limiter où ses contrôles se connectent avec `port_targets` dans son `agent.toml`, redirections comprises ; partout par défaut.

## Mise en place

1. Ouvrez **Fleet** et cliquez sur le serveur qui fera le contrôle : le serveur web, ou un autre qui atteint le site.
2. Dans son onglet **« Sensors »**, choisissez **« Add a check »**.
3. Dans **« Find a check »**, tapez `site` et choisissez **« Un site web répond »**.
4. Dans **« Adresse de la page »**, saisissez l'adresse comme la tape un visiteur, par exemple `https://www.example.com/`. Dans **« Texte que la page doit contenir »**, saisissez un texte qui n'apparaît que quand la page fonctionne, comme `Ajouter au panier`, ou laissez vide.
5. Laissez **« Raise critical when it fails »** coché, choisissez **« This server only »** sous **« Add to »**, et cliquez sur **« Add check »**.

**« Plus d'options »** contient le reste : un texte que la page ne doit pas contenir, les codes de statut acceptés (`200-299` par défaut), le suivi des redirections, `GET` ou `HEAD`, IPv4 ou IPv6, des en-têtes supplémentaires, et un nom d'utilisateur et un mot de passe pour une page protégée. Le mot de passe n'est envoyé qu'en https, gardé scellé et jamais réaffiché ; sans nom d'utilisateur, il est envoyé comme jeton bearer, comme beaucoup d'API l'attendent.

La liste **« Sites web »** de l'onglet **« Sensors »** montre ensuite chaque site : s'il répond, pourquoi sinon, son code de statut, son temps de réponse, où mènent ses redirections, et son certificat avec ses jours restants, sa date d'expiration et qui l'a émis.

## Modifier l'alerte

Dans l'onglet « Sensors », ouvrez le menu de la ligne et choisissez « Edit ». « Alert » choisit critique ou avertissement, « For at least » le nombre de minutes que doit durer le problème (`0` alerte dès la première mauvaise valeur), et « Apply to » ce serveur seul ou tous ceux du site.

## Où part l'alerte

Chaque alerte s'affiche dans « Monitoring › Incidents » et sur la vue d'ensemble. Pour être prévenu, ajoutez un canal dans « Settings › Notifications » : e-mail, webhook, Slack, Teams ou Discord, pour tout, un client ou un site ; « Send test » le vérifie. Une alerte critique non acquittée en 15 minutes est aussi transmise aux administrateurs les plus proches qui ont indiqué comment les joindre sur la page « On call ».

## Quand l'alerte se déclenche

```
Critical: http_www_example_com_up is 0 on web-1 (paris)
Warning: http_www_example_com_cert_days_left is 12 on web-1 (paris)
```

Par e-mail, l'objet reprend cette ligne après `[MonitorFlow]`, avec un lien vers l'alerte. Cliquez sur « Acknowledge », puis vérifiez :

1. **D'où vient l'échec :** si seule la vérification depuis un autre serveur échoue, regardez le réseau, le pare-feu ou le DNS entre les deux.
2. **Le serveur web :** voir [nginx](surveiller-nginx.md) ou [Apache](surveiller-apache.md).
3. **Ce qu'il dit :** la liste **« Sites web »** dit pourquoi le dernier contrôle a échoué, par exemple « A répondu 503, qui n'est pas attendu », et ce qui ne va pas avec le certificat. Vérifiez que le renouvellement s'exécute et que le serveur web a été rechargé ensuite.

## Pas encore de MonitorFlow ?

MonitorFlow est libre et gratuit. Installez le serveur central sur une machine Linux en une ligne :

```sh
curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install-server.sh | sudo sh
```

Ouvrez ensuite « Settings › Add servers » dans l'interface web et collez la commande affichée sur chaque serveur. Voir [Installer le serveur](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-server.md) et [Installer l'agent](https://github.com/Kidoly/monitorflow/blob/main/docs/deploy-agent.md) (en anglais).
