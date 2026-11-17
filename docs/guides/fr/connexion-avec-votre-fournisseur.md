---
title: "Comment se connecter à MonitorFlow avec votre propre fournisseur d'identité"
description: "Laissez vos collègues se connecter à MonitorFlow avec les comptes et le second facteur qu'ils ont déjà : OpenID Connect (Entra ID, Google, Keycloak, authentik) ou un annuaire LDAP en TLS."
slug: connexion-avec-votre-fournisseur
lang: fr
translation: sign-in-with-your-identity-provider
---

# Comment se connecter à MonitorFlow avec votre propre fournisseur d'identité

Vos collègues ont déjà un compte, un mot de passe et un second facteur chez le fournisseur d'identité de votre entreprise. MonitorFlow peut s'en servir : chacun se connecte avec ce compte, son rôle dans MonitorFlow vient de ses groupes, et une personne qui quitte le groupe, ou l'entreprise, perd l'accès sans que personne ne touche à MonitorFlow.

## Ce qu'il vous faut

- Un administrateur de tout dans MonitorFlow.
- L'un de ces fournisseurs :
  - un fournisseur **OpenID Connect** : Microsoft Entra ID, Google, Okta, Keycloak, authentik, Zitadel ou tout autre qui suit la norme ;
  - un **annuaire LDAP** joignable en **LDAPS** (port 636) ou en **StartTLS** (port 389) : Active Directory, OpenLDAP, FreeIPA. Le LDAP en clair est refusé.
- Les groupes qui doivent donner un rôle dans MonitorFlow, comme `monitorflow-admins` et `helpdesk`.

Votre propre compte MonitorFlow garde son mot de passe : c'est l'**administrateur local**, le moyen d'entrer quand le fournisseur est en panne. MonitorFlow en garde toujours au moins un.

## Ajouter un fournisseur OpenID Connect

1. Dans MonitorFlow, ouvrez **Paramètres › Connexion** et choisissez **Ajouter un fournisseur OpenID Connect**. Copiez l'**URI de redirection** affichée, `https://<votre MonitorFlow>/api/auth/oidc/callback`.
2. Chez votre fournisseur, créez un client (une application) pour MonitorFlow :
   - **Keycloak** : Clients › Create client, type OpenID Connect, authentification du client activée, flux standard seulement. Collez l'URI de redirection. Dans Client scopes › portée dédiée, ajoutez un mapper « Group Membership » nommé `groups`, sans le chemin complet des groupes.
   - **authentik** : Applications › Create with provider, OAuth2/OpenID, confidentiel, URI de redirection en mode strict, une clé de signature (RS256). La portée `profile` envoie déjà les groupes.
   - **Entra ID** : App registrations › New registration, URI de redirection web, puis Token configuration › Add groups claim (groupes de sécurité) et un secret client.
3. De retour dans MonitorFlow, saisissez un **nom** (le bouton dira « Se connecter avec » ce nom), l'**adresse de l'émetteur**, l'**identifiant client** et le **secret client**. L'émetteur est l'adresse du document de découverte du fournisseur, sans `/.well-known/openid-configuration` : par exemple `https://id.example.com/realms/acme` pour Keycloak ou `https://auth.example.com/application/o/monitorflow/` pour authentik.
4. Si le certificat de votre fournisseur vient d'une autorité de certification privée, collez cette autorité sous **Avancé**.
5. Choisissez **Tester la connexion**. MonitorFlow lit le document de découverte et les clés de signature, et vérifie l'identifiant et le secret client. Choisissez ensuite **Enregistrer et activer** : ce n'est possible qu'une fois le test réussi.

## Ajouter un annuaire LDAP

1. Ouvrez **Paramètres › Connexion** et choisissez **Ajouter un annuaire LDAP**. Choisissez **Active Directory** ou **OpenLDAP et autres** : les filtres suivent.
2. Saisissez l'**adresse de l'annuaire** (`ldaps://dc1.example.com`, ou `ldap://dc1.example.com` avec StartTLS), un **compte de service** qui peut chercher les personnes et les groupes, son mot de passe, la **base des personnes** et la **base des groupes**. Laissez **Compter les groupes contenus dans des groupes** activé si vos groupes en contiennent d'autres.
3. Choisissez **Tester la connexion**. Pour voir ce qu'une personne obtiendrait, saisissez son nom d'utilisateur et son mot de passe de l'annuaire sous **Essayer une personne** : le test affiche ses groupes et les rôles qu'elle aurait. Rien n'est enregistré.
4. Choisissez **Enregistrer et activer**.

Les personnes d'un annuaire se connectent avec le formulaire habituel, avec leur nom d'utilisateur et leur mot de passe de l'annuaire.

## Donner des rôles aux groupes

Sous **Groupes et rôles**, ajoutez une ligne par groupe : son nom tel que le fournisseur l'envoie, et un rôle (lecteur, opérateur, approbateur ou administrateur). Un MSP choisit aussi où le rôle s'applique : partout, un client ou un site.

- Une personne qui n'est dans aucun de ces groupes ne peut pas se connecter.
- Les rôles suivent les groupes à chaque connexion. Quand vous changez cette liste, tous ceux qui se connectent par ce fournisseur reçoivent leurs nouveaux rôles tout de suite, et ceux qui n'en ont plus aucun sont déconnectés.
- Leurs comptes sont créés à leur première connexion. Sous **Paramètres › Utilisateurs et rôles**, ils indiquent « Se connecte avec » le fournisseur ; leurs rôles ne se changent pas à la main, et ils n'ont pas de mot de passe dans MonitorFlow.

## En cas de problème

- **« Le fournisseur de connexion est injoignable »** : le fournisseur est en panne ou son certificat a changé. Connectez-vous avec l'administrateur local, puis relancez **Tester la connexion**.
- **« Votre compte n'est dans aucun des groupes qui donnent accès »** : ajoutez la personne à un groupe sous **Groupes et rôles**, ou vérifiez le nom du groupe chez le fournisseur.
- **« Un autre compte porte déjà votre nom »** : MonitorFlow ne rattache jamais le compte d'un fournisseur à un compte existant par son nom, car le fournisseur pourrait alors le prendre. Renommez ou supprimez l'autre compte.
- Chaque changement de ces paramètres, et chaque connexion par un fournisseur, figure dans le journal d'audit.
