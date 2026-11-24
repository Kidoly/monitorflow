---
title: "Comment recevoir les alertes MonitorFlow sur votre téléphone"
description: "Installez MonitorFlow sur l’écran d’accueil de votre téléphone, recevez des notifications push pendant l’astreinte, et prenez en charge ou mettez en silence une alerte en deux gestes, sans store ni service extérieur."
slug: alertes-sur-votre-telephone
lang: fr
translation: alerts-on-your-phone
---

# Comment recevoir les alertes MonitorFlow sur votre téléphone

Pendant l’astreinte, l’alerte doit vous réveiller sur votre téléphone, et la traiter doit tenir en deux gestes : dire que vous vous en occupez, ou la mettre en silence pour une heure. L’interface web de MonitorFlow s’installe sur un téléphone comme une application, vous garde connecté pendant des semaines, et envoie des notifications push par le service push de votre téléphone. Pas de store, pas de compte chez nous, pas de relais de notre part.

## Ce que vous obtenez

- **Des notifications** sur Android (Chrome, Firefox, Edge) et sur iPhone et iPad (iOS et iPadOS 16.4 ou plus récent), chaque fois que vous recevriez un e-mail ou un message de messagerie d’astreinte : pendant vos tours, quand une relance vous nomme, et en tant qu’administrateur pendant une période non couverte d’un endroit que vous administrez.
- **Prendre en charge** depuis la notification elle-même sur Android ; sur iOS, un appui ouvre l’alerte avec son bouton Prendre en charge.
- **Mettre en silence** pendant 30 minutes, 1, 4 ou 24 heures : seulement le contrôle de l’alerte, ou tout son serveur.
- **Des pages faites pour un téléphone** : alertes ouvertes, une alerte, qui est d’astreinte maintenant et ensuite, et vos téléphones.
- **Une session longue** : 90 jours, ou 30 jours sans utilisation, sur ce téléphone seulement. Retirez le téléphone et il est déconnecté aussitôt.

## Avant de commencer

- Le serveur central doit joindre le service push de votre téléphone en **HTTPS sortant** (port 443) : `web.push.apple.com` et ses sous-domaines pour iPhone et iPad, `fcm.googleapis.com` pour Chrome sur Android, `updates.push.services.mozilla.com` pour Firefox, `*.notify.windows.com` pour Edge. Il s’y connecte directement, pas par un proxy. Sans cela, tout ce qui suit fonctionne sauf les notifications elles-mêmes, et la page du téléphone indique que le service push est injoignable.
- Votre téléphone doit ouvrir l’adresse de l’interface web, sur le réseau local, un VPN ou Internet, et faire confiance à son certificat (voir la page de confiance de votre serveur).
- Pour être prévenu, vous devez être dans un planning d’astreinte (Surveillance › Astreinte). Une équipe où chacun veut toutes les alertes peut faire un planning avec tout le monde « d’astreinte en même temps ».

## Mise en place

1. Sur le téléphone, ouvrez l’adresse de MonitorFlow et connectez-vous, avec votre mot de passe ou votre fournisseur d’identité.
2. Ajoutez-le à l’écran d’accueil :
   - **iPhone, iPad** : Partager, puis **Sur l’écran d’accueil**. Ouvrez ensuite toujours MonitorFlow depuis l’écran d’accueil : iOS n’envoie de notifications que là.
   - **Android** : le menu du navigateur, puis **Installer l’application** ou **Ajouter à l’écran d’accueil**.
3. Ouvrez l’application et allez dans **Ce téléphone**. Vérifiez son nom et appuyez sur **Ajouter ce téléphone**.
4. Appuyez sur **Activer les notifications** et autorisez-les quand le téléphone le demande.
5. Appuyez sur **Envoyer un test**. Une notification doit s’afficher en quelques secondes.

La page **Astreinte** de l’interface complète indique, sous « Comment vous joindre », combien de vos téléphones reçoivent les notifications, et son bouton **Envoyer un test** les teste aussi.

## À quoi ressemble une notification

> **MonitorFlow**
> Critical: disk_used_percent /var is 97.25 on web-1 (paris)
> [Prendre en charge] [Mettre en silence…]

Elle dit quoi et où, jamais un mot de passe ni un lien qui agit tout seul. Plusieurs alertes envoyées ensemble font une seule notification qui ouvre leur liste, et vous en recevez au plus trois par minute : la troisième dit combien il en reste. Quand l’alerte se résout, sa notification est remplacée par la résolution.

- **Prendre en charge** dit au serveur que vous vous en occupez, avec la session de votre téléphone : personne d’autre n’est prévenu ensuite. Si le téléphone était déconnecté, l’alerte s’ouvre pour que vous vous connectiez.
- **Mettre en silence…** ouvre l’alerte avec les choix : ce contrôle ou tout le serveur, et pour combien de temps. L’alerte reste ouverte et visible ; ses notifications s’arrêtent jusqu’à la fin, et ce qui est encore ouvert alors est envoyé.

## Vos téléphones

**Ce téléphone** liste chaque téléphone ajouté, sa dernière utilisation et si ses notifications sont activées. **Retirer** déconnecte ce téléphone et arrête ses notifications ; faites-le depuis un autre téléphone si l’un est perdu. Un administrateur qui désactive votre compte met fin à toutes les sessions aussitôt, téléphones compris, et changer votre mot de passe déconnecte vos autres sessions, téléphones aussi.

## Quand ça ne marche pas

- **« Ajoutez d’abord MonitorFlow à l’écran d’accueil »** : sur iPhone et iPad, Safari ne propose les notifications qu’à l’application ouverte depuis l’écran d’accueil.
- **« Les notifications sont bloquées »** : autorisez-les pour MonitorFlow dans les réglages du téléphone (ou les réglages du site dans le navigateur), puis appuyez de nouveau sur Activer les notifications.
- **« Could not connect to …push… » ou « No answer from … »** : le serveur central ne joint pas le service push ; ouvrez-lui le HTTPS sortant dans le pare-feu.
- **« The push service no longer knows this phone »** : le navigateur a abandonné son abonnement (données effacées, réinstallation). Ouvrez l’application sur ce téléphone et activez de nouveau les notifications.
- **Rien n’arrive, sans erreur** : vérifiez que vous êtes d’astreinte maintenant (onglet Astreinte), qu’aucun silence ne couvre l’alerte, et que le téléphone n’est pas en mode concentration ou ne pas déranger.

## Pour les administrateurs

Le serveur signe ses notifications avec sa propre clé, créée au premier usage et gardée scellée dans la base, si bien que tous les serveurs centraux d’une grappe envoient avec la même. Rien ne passe par un service à nous : Apple, Google, Mozilla ou Microsoft transportent un message chiffré qu’ils ne peuvent pas lire. Chaque changement d’un téléphone est dans le journal d’audit (`user.phone_add`, `user.phone_push_on`, `user.phone_push_off`, `user.phone_rename`, `user.phone_remove`), comme les prises en charge et les silences, d’où qu’ils viennent. Des applications natives dans les stores sont prévues ; la conception est dans l’[ADR-0186](../../adr/0186-phones-and-push-notifications.md).
