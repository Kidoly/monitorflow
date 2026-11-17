---
title: "How to sign in to MonitorFlow with your own identity provider"
description: "Let your colleagues sign in to MonitorFlow with the accounts and second factor they already have: OpenID Connect (Entra ID, Google, Keycloak, authentik) or an LDAP directory over TLS."
slug: sign-in-with-your-identity-provider
lang: en
translation: connexion-avec-votre-fournisseur
---

# How to sign in to MonitorFlow with your own identity provider

Your colleagues already have an account, a password and a second factor in your company's identity provider. MonitorFlow can use them: people sign in with that account, their role in MonitorFlow comes from their groups, and someone who leaves the group, or the company, loses access without anyone touching MonitorFlow.

## What you need

- A global administrator of MonitorFlow (an administrator of everything).
- One of:
  - an **OpenID Connect** provider: Microsoft Entra ID, Google, Okta, Keycloak, authentik, Zitadel or any other that follows the standard;
  - an **LDAP directory** reachable over **LDAPS** (port 636) or **StartTLS** (port 389): Active Directory, OpenLDAP, FreeIPA. Plain LDAP is refused.
- The groups that should get a role in MonitorFlow, such as `monitorflow-admins` and `helpdesk`.

Your own MonitorFlow account keeps its password: it is the **local administrator**, the way in when the provider is down. MonitorFlow always keeps at least one.

## Add an OpenID Connect provider

1. In MonitorFlow, open **Settings › Sign-in** and choose **Add OpenID Connect provider**. Copy the **Redirect URI** it shows, `https://<your MonitorFlow>/api/auth/oidc/callback`.
2. In your provider, create a client (an application) for MonitorFlow:
   - **Keycloak**: Clients › Create client, type OpenID Connect, client authentication on, standard flow only. Paste the redirect URI. Under Client scopes › dedicated scope, add a "Group Membership" mapper named `groups` with "Full group path" off.
   - **authentik**: Applications › Create with provider, OAuth2/OpenID, confidential, redirect URI in strict mode, a signing key (RS256). The `profile` scope already sends the groups.
   - **Entra ID**: App registrations › New registration, web redirect URI, then Token configuration › Add groups claim (security groups) and a client secret.
3. Back in MonitorFlow, fill in a **name** (the button will say "Sign in with" it), the **issuer address**, the **client ID** and the **client secret**. The issuer is the address of the provider's discovery document without `/.well-known/openid-configuration`, for example `https://id.example.com/realms/acme` for Keycloak or `https://auth.example.com/application/o/monitorflow/` for authentik.
4. If your provider's certificate comes from a private certificate authority, paste that authority under **Advanced**.
5. Choose **Test connection**. MonitorFlow reads the discovery document and the signing keys, and checks the client ID and secret. Then choose **Save and turn on**: it is only possible once the test passed.

## Add an LDAP directory

1. Open **Settings › Sign-in** and choose **Add LDAP directory**. Pick **Active Directory** or **OpenLDAP and others**: the filters follow.
2. Fill in the **directory address** (`ldaps://dc1.example.com`, or `ldap://dc1.example.com` with StartTLS), a **service account** that may search people and groups, its password, the **people base** and the **groups base**. Keep **Count groups inside groups** on if your groups contain other groups.
3. Choose **Test connection**. To see what a person would get, type their directory username and password under **Try a person**: the test shows their groups and the roles they would have. Nothing is saved.
4. Choose **Save and turn on**.

People of a directory sign in with the usual form, with their directory username and password.

## Give roles to groups

Under **Groups and roles**, add one line per group: its name as the provider sends it, and a role (viewer, operator, approver or administrator). An MSP also picks where the role applies: everything, a customer or a site.

- Someone in none of these groups cannot sign in.
- Roles follow the groups at each sign-in. When you change this list, everyone who signs in through the provider gets their new roles at once, and those left without any are signed out.
- Their accounts are created at their first sign-in. Under **Settings › Users and roles** they show "Signs in with" the provider; their roles cannot be changed by hand, and they have no password in MonitorFlow.

## When something goes wrong

- **"The sign-in provider could not be reached"**: the provider is down or its certificate changed. Sign in with the local administrator, then run **Test connection** again.
- **"Your account is in none of the groups that give access"**: add the person to a group under **Groups and roles**, or check the group's name in the provider.
- **"Another account already has your name"**: MonitorFlow never joins a provider's account to an existing account by its name, as that would let the provider take it over. Rename or remove the other account.
- Every change of these settings, and every sign-in through a provider, is in the audit log.
