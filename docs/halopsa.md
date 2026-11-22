# HaloPSA

MonitorFlow can open a HaloPSA ticket for an incident and close it when the incident is resolved (ADR-0164, ADR-0166). This page says how to set it up and what the first version does.

The connection is set up by a global administrator under Settings › Integrations › HaloPSA, or through the public API (`/api/v1/psa`, tag `psa` in the API documentation).

## Create the API application in HaloPSA

1. In HaloPSA, create an agent for MonitorFlow, so that what MonitorFlow does in Halo is attributed to it. Give it only the permissions it needs: read clients and sites, create and edit tickets.
2. Go to Configuration › Integrations › HaloPSA API › View Applications, and add an application.
3. Authentication method: **Client ID and Secret (Client Credentials)**. Login type: agent, the agent from step 1.
4. Under Permissions, tick **all**: MonitorFlow asks for a token with the scope `all`.
5. Save, then copy the **Client ID** and the **Client Secret**. Halo shows the secret once.

The address to enter in MonitorFlow is the one of your Halo web interface, like `https://halo.example.com`, without `/api`. MonitorFlow signs in on `https://halo.example.com/auth/token` and calls `https://halo.example.com/api`. A hosted instance whose sign-in needs it also takes its tenant name.

**Test connection** signs in and reads the application's agent, without changing anything in Halo. The secret is sealed in the database (ADR-0125) and never shown again; to change it, enter a new one.
