# ADR-0104: Customers out of sight for a single company

Date: 2026-11-18
Status: accepted

## Context

MonitorFlow serves two audiences with one product (`docs/product.md`, "Who it is for"): a company running its own servers, and an MSP running servers for many customers. Customers (ADR-0026) are the MSP's tool: they keep servers, sites, notifications and users apart. A company has exactly one, created by the first migration as `default`, and has no use for the word.

Before this change the web interface showed the MSP's machinery to everyone: a customer picker at the top of the sidebar ("All customers", then `default`), "default › paris" in install commands and site lists, a "Customer" fact on campaigns, "Customer default" in silence and dashboard choices, "All of default" in roles, "Customer: default" in alert emails. A few pages hid it already, each by counting customers in its own way, and none hid it from an MSP's customer whose user sees only one.

The rule in the product description is: a company with one organisation sees no customers, no customer picker and no MSP vocabulary; customers appear only once a second one exists; an MSP's customer sees their own servers in plain words, never the MSP's other customers or its internal tools.

## Decision

**One flag, computed by the server.** `GET /api/auth/me`, and the answers of login, setup and setting a password, which return the same body, gain `multi_customer`: whether the user sees more than one customer, counted as `GET /api/customers` lists them. A global administrator sees every customer, so the flag is true as soon as a second one exists; a user limited to one customer, or to sites of one, gets false whatever the install holds. Nothing else in the API changes: lists keep their `customer_id` and `customer_name`, so scripts and integrations read the same answers.

**The web interface asks the flag, never the data.** `useMultiCustomer()` (`web/src/lib/me.ts`) is the only test a component makes. No page counts customers or guesses from names. An older server without the field reads as true, the behaviour before this change.

**Without it (one customer):**

- No customer picker. The sidebar picker lists sites ("All sites", then each site) when there are two or more, and is not shown for one. It still writes `?customer=&site=`, so links keep working when customers appear.
- No customer in names: "paris", not "default › paris" or "acme / paris"; no "Customer" column, fact or line.
- A choice that covers the one customer as a whole (a silence, a dashboard's sharing, a role, a channel's "Notify about", an on-call schedule, a follow-up) reads "All sites". A global administrator's "Everything" stays, and the customer-wide choice is not offered next to it, as both mean the same to them.
- Notification emails and chat messages leave the customer out. Webhook payloads keep it: they are read by programs.
- Sites stay, in plain words: a company may have several.

**With it (two customers or more)**, every page shows what it showed before: the customer picker, customer names next to sites, customer choices. Nothing to migrate or turn on: creating the second customer is the switch, and the creator's interface changes at once, as the page reloads the user after creating it.

**Adding a customer is quiet.** Only a global administrator may (ADR-0026). It is in Administration › Sites, at the bottom, behind "Manage servers for several customers?", and never in the main navigation. The CLI (`monitorflow-server customer create`) is unchanged.

**For new features.** A component that writes "customer" on screen, or shows a customer's name, does so only when `useMultiCustomer()` is true, and words the same thing for a single company without it. A test in `web/src/lib/customerWording.test.ts` fails when a component has customer wording in a string or JSX text and does not read `multiCustomer`; files that need it unconditionally (the customer administration itself) are listed there with the reason. Server texts meant for people (notifications) follow the same rule with `customers::several`.

Rejected:

- **A setting "I am an MSP".** One more thing to set, and wrong the day a company takes on a second organisation; the second customer already says it.
- **Each page counting customers.** It is how it was, and each page drew the line differently. It also cannot work for a customer's user: they must not learn how many customers exist.
- **Renaming `default` to the company's name at setup.** It would still show as a customer everywhere; the name is not the problem, the machinery is.
- **Removing customers from API answers for single companies.** It would break integrations the day a second customer is added. The API is for programs; the words are the interface's job.

## Consequences

- A single company sees sites, never customers, in the web interface and in alerts; an MSP sees everything as before; an MSP's customer sees their own servers and sites only, without the MSP vocabulary.
- `/api/auth/me` costs one more small query per call.
- The flag is read once per page load, and again after creating a customer in the same browser; another administrator's browser shows customers at its next page load.
- A customer named `default` is still left out of notification texts among several customers, as before.
- Not in this change: deleting a customer to go back to one (the API has no route for it; the interface follows if one is added), and the CLI's wording, which is for administrators setting up an MSP.
