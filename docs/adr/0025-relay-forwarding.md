# ADR-0025: Relay forwarding model

Date: 2026-10-19
Status: accepted

## Context

A relay sits on an isolated site and forwards its agents' streams to the central server, or later to another relay (architecture §2). The pieces it needs already exist on the server. A stream opened with a `relay` certificate whose `Hello.agent_id` is not the relay's own ID is a relayed agent stream. The server accepts it only if `DbRelayPolicy` says the agent belongs to the relay's site (ADR-0004, ADR-0008), records `via_relay`, and registers it like a direct stream. What was still open:

- how the relay authenticates its agents, and how agent streams map to upstream streams;
- which certificate the relay presents to its agents. Peer certificates carried a URI SAN and `clientAuth` only, and agents check the upstream's certificate against the host of `upstream_url` like any TLS client, so a relay certificate could not serve a listener;
- what the relay does when its upstream is down, before store-and-forward exists.

## Decision

**The relay terminates its agents' mTLS.** Its listener requires a client certificate that chains to the MonitorFlow CA and is not in the CA's revocation list. The certificate must have role `agent`; downstream relays are refused until relay chaining exists. The first envelope must be a `Hello` within 10 s with `agent_id` equal to the certificate ID, otherwise the stream is refused with `PERMISSION_DENIED` or `INVALID_ARGUMENT`, as the server does for a direct agent. The relay checks nothing else in the Hello and interprets no other envelope.

**The CRL comes from the upstream.** A new unary RPC, `RevocationService.GetCrl`, served next to `CertificateService` on the mTLS listener, returns the DER CRL the server's listener enforces. Only relay certificates may call it; any relay may, as revocations are not secret. The relay fetches it before it starts listening, then every hour, retrying every minute after a failure. Like the server, the relay enforces the CRL's `next_update` (24 h validity, refreshed every 6 h by the server), so a relay cut off from the server for about a day refuses its agents. Without store-and-forward that costs nothing more: an unreachable upstream refuses agents anyway (below). An envelope on the stream was rejected for the CRL, because envelopes are agent traffic that relays do not interpret; a unary RPC follows the rule of architecture §3.

**Relay certificates name the relay's addresses.** An enrollment token for a relay carries the DNS names and IP addresses its agents use (`token create --role relay --server-name relay1.paris.example`, stored in `enrollment_tokens.server_names`). The CA writes them as SANs and adds `serverAuth` to the certificate. As with the identity (ADR-0007), the names come from the administrator, never from the CSR, so a relay cannot obtain a certificate for the server's name. Renewal copies the names of the presented certificate, which the CA signed from the token. Agent tokens and agent certificates still carry none.

**One upstream stream per agent stream, each on its own connection.** For each accepted agent stream the relay dials `upstream_url` with its own certificate, opens `AgentService.Stream`, sends the agent's Hello unchanged and then pipes envelopes unchanged both ways: same sequence numbers, same payloads, nothing logged but IDs and counts. The server keeps its per-agent logic unchanged: policy check, registry entry with `via_relay`, sequence tracking, heartbeat timeout, outbound queue and sensor configuration push. Commands and sensor configurations queued for the agent go down that stream and the relay passes them on. Backpressure is end to end: a bounded queue upstream and HTTP/2 flow control both ways. A separate connection per stream costs one TLS 1.3 handshake per agent connect. In exchange, a broken connection affects one agent, no per-connection stream limit applies (hyper's server default is 200 concurrent streams), and the server sees the same number of connections as with direct agents, which it is sized for.

**Statuses go down as the upstream sent them.** If the upstream refuses the Hello or later ends the stream with a status, the agent gets that code and message, so it applies its own reconnect policy (ADR-0009). Examples: `PERMISSION_DENIED` "relay:paris-1 may not speak for agent web-02" for an agent of another site, `ABORTED` for a replaced connection, `DEADLINE_EXCEEDED` for a heartbeat timeout. Failures of the relay's own link (connect, TLS handshake, transport error) become `UNAVAILABLE` "upstream unavailable", with the details in the relay's log only. The agent looks for TLS alert wording in error text, so passing it on would make an agent read a refusal of the relay's certificate as a refusal of its own.

**Liveness follows the downstream.** When the agent's stream ends or fails, the relay ends the upstream stream, and the server removes the registry entry: the agent shows offline. The relay's listener sends HTTP/2 keepalives (30 s, 20 s timeout) to detect a silent agent, and the server's heartbeat timeout remains the backstop. Agents keep sending their heartbeats through the relay.

**The relay has its own stream.** Like an agent, the relay keeps one `AgentService.Stream` open as itself (`Hello.agent_id` = its ID, no OS fields). The server updates `relays.last_seen_at` and lists it in the registry. It reconnects with the policy of ADR-0009. While that stream is not open, the relay refuses new agent streams with `UNAVAILABLE` without dialing, so agents back off. It does not close forwarded streams: each of those has its own connection and ends by itself if the upstream really is gone.

**No disk buffer yet.** A down upstream means refused or closed agent streams; agents keep their samples in memory meanwhile (ADR-0016's uplink queue). Store-and-forward is the next roadmap task and will change this paragraph.

**Relay identity.** The relay enrolls with a relay token and stores its files under `<data_dir>/tls/` unless `[tls]` paths are set, like the agent and the server (ADR-0007). It renews its certificate as in ADR-0010, with the client code shared with the agent. A renewed certificate applies to new agent connections, new upstream dials and the relay's own stream, which reconnects at once. Open forwarded streams keep the connection they have; the old certificate stays valid until it expires (ADR-0010).

**Chaining stays possible.** The upstream may be a relay: it would accept a downstream relay's certificate and forward its streams unchanged, and the server's policy would check the last relay's site. That needs a relay-to-relay policy, which is the relay chaining task.

## Alternatives rejected

- **One multiplexed upstream stream per relay**, with the agent ID added to each envelope. It needs protocol changes, per-agent sub-sessions in the server stream (registry, sequences, policy per Hello), its own backpressure so one slow agent does not stall the others, and it breaks the rule that relays do not interpret envelopes.
- **All agent streams on one shared HTTP/2 connection.** It is cheaper in handshakes, but it hits the server's per-connection stream limit, and one connection failure drops every agent at once. It can come back later as a pool of connections if handshakes become a cost.
- **No CRL on the relay**, relying on the server refusing revoked inventory rows. A certificate can be revoked without its row (a compromised key), and the server never sees the agent's certificate behind a relay.
- **Agents trusting any CA-signed `relay` or `server` certificate, whatever the name.** Any enrolled relay could then pose as the server to agents of every site.

## Consequences

- Operators create relay tokens with `--server-name` for every name agents use. A relay enrolled without names cannot accept agents and says so at start. Tokens created through the REST API get no names until the relay administration task adds them.
- Agents need no change: they point `upstream_url` at the relay, and `enroll_url` still at the server's enrollment listener.
- An agent behind a relay cannot renew its certificate yet: the relay does not serve `CertificateService`, so the agent gets `UNIMPLEMENTED` and retries with backoff. Forwarding `Renew` under the relay policy is a follow-up (ADR-0010).
- The server handles one connection per relayed agent, as for direct agents.
- A relay cut off for about a day, or unable to fetch the CRL, refuses its agents until it fetches a fresh one.
