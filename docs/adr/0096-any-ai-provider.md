# ADR-0096: Any AI for remediation, local models included

Date: 2026-11-16
Status: accepted

## Context

ADR-0003 and ADR-0039 promised one trait with one adapter per provider, Claude first, and only Claude was ever built. The AI page (ADR-0064) offered one thing: an Anthropic key. People read that as a bias, and some cannot send incident data outside their network at all, so they had no AI. Product's non-goals said a local model was planned for Phase 15.

Three kinds of servers cover nearly everyone: Anthropic, OpenAI, and the many that speak OpenAI's Chat Completions API: Ollama, llama.cpp's server, vLLM, LM Studio, LiteLLM, Mistral, OpenRouter, Azure OpenAI's v1 endpoint. Local models are the reason for the third and its main difficulty: small models call tools poorly, often write the call as text, sometimes write anything.

What must not move: classification by rules, forbidden commands, approvals, snapshots and the audit chain apply to whatever the model says (architecture §8).

## Decision

**Three providers, two adapters.** `[ai] provider` is `anthropic` (the existing Claude adapter, `claude` still accepted), `openai`, or `openai_compatible` with a `base_url` like `http://ollama.lan:11434/v1`. The last two share one adapter, `crates/ai/src/openai.rs`, in two flavours:

- **OpenAI**: strict function tools, `tool_choice: "required"`, `parallel_tool_calls: false`, and `max_completion_tokens`, since OpenAI's reasoning models refuse `max_tokens`. A key is required.
- **Compatible**: only what such servers share: tools with `tool_choice: "auto"` (`required` is not understood everywhere) and `max_tokens`. The key is optional and sent as a bearer token when given. Azure OpenAI works through its v1 endpoint (`https://NAME.openai.azure.com/openai/v1`) with its key; nothing Azure-specific is built.

Each step stays one stateless request (ADR-0039). The server's prompt and the adapter's rules are the system message. The context and earlier steps, which Chat Completions cannot send as documents, are one pretty-printed JSON value in the user message: quotes and line breaks in a log are escaped, so text in the data cannot end it early and pose as instructions.

**Answers, parsed the same strict way.** The checks of ADR-0039 move to `answer.rs`, shared by both adapters: one call to `propose_command` or `stop`, no unknown or missing field, no risk field, non-empty text, a one-line command of at most 4096 bytes. What the OpenAI adapter accepts as that call:

1. one tool call, its arguments a JSON string as the API defines them or an object as some servers send;
2. else one JSON object in the text, as the compatible flavour's rules ask when the model cannot call tools: `{"tool": "propose_command", ...fields}` (`action` also names the tool), or a call written out, `{"name": ..., "arguments": {...}}`, which models print when the server does not parse their calls. Thinking before `</think>`, code fences and prose around the object are ignored; two objects do not parse.

Anything else, two tool calls included, is malformed. A malformed answer is asked again once, the user message saying what was wrong; a second one is `AiError::Malformed`, and the incident fails with the reason in its history, as with Claude, before anything is classified or run. `finish_reason: length` is `Truncated` and a refusal or `content_filter` is `Refused`, neither asked again.

When a compatible server refuses tools (HTTP 400, 422 or 501 whose message speaks of tools, like Ollama's "does not support tools" or vLLM without a tool parser), the adapter asks again without tools and keeps asking for JSON from then on. Nobody has to know whether their model or server does tools.

Rejected: JSON-only for every compatible server, which throws away the reliable tool calling of good models; `response_format` JSON modes, which servers implement differently; replaying the conversation as assistant and tool turns, rejected in ADR-0039 already.

**Plain HTTP only to private addresses.** Ollama, llama.cpp and vLLM serve plain HTTP; asking for a TLS proxy in front of a model on the next machine would keep most people from using one. A compatible base URL may therefore be `http://`, but only when every address of its host is loopback, private (RFC 1918), link-local, shared (100.64.0.0/10, as Tailscale uses) or unique local (fc00::/7). A name is resolved before each request, so one that moves to a public address stops being used rather than sending data in clear across the internet. Anything else must be `https://`. Like the mirror's plain HTTP (ADR-0030), this is outgoing and to a machine the administrator chose; it adds no listener. The data sent is the redacted context, as to any provider. Base URLs holding a user name, password, query or fragment are refused, and no adapter follows redirects, so a key never goes to another host.

**Limits per provider, nothing to set.** `max_tokens` defaults to 16000 for Anthropic and OpenAI, room for thinking, and 4096 for compatible servers: vLLM refuses a request whose prompt and `max_tokens` exceed the model's context, often small locally, and the answer is a short call. `request_timeout` stays 300 seconds for all; a model on a CPU is slow, not hung. The Test allows 120 seconds instead of 30 for a compatible server, which may load the model from disk first. Retries are those of ADR-0039 for every provider. The context itself is bounded by the server whatever the model (ADR-0040); a local model needs a context window of about 32,000 tokens to see all of it, which the page says, with Ollama's `OLLAMA_CONTEXT_LENGTH`. `[ai] max_tokens` and `request_timeout` still apply to settings made in the web interface.

**The page** (Administration › AI, ADR-0064) asks first which AI, in plain words: "Anthropic (Claude)", "OpenAI", "Another or a local model". Then only what that one needs:

- Anthropic: key, and the short list of models as before.
- OpenAI: key, and the model, typed or picked from the models the key can use, fetched from `/v1/models` and filtered to those that chat. It starts at `gpt-5.1`.
- Another or a local model: the address, the model, picked from what the server lists when it lists them, and a key only if the server asks for one. A plain `http://` address shows that it is sent unencrypted on the local network; a public one is refused with what to write instead.

Test and Enable work as before. The key is stored and never shown again, as before; it belongs to the provider it was entered for, so choosing another provider forgets it unless a new one is entered (one key column, no key of one provider ever sent to another). `GET /api/settings/ai` adds `provider` and `base_url`; `PUT` takes them; `POST /api/settings/ai/models` lists models for the form's provider, address and key, or for the stored ones, global administrators only. The migration adds `ai_provider` and `ai_base_url` and lets a compatible provider be enabled without a key, but not without an address.

**Honest about quality.** The page says, under "Another or a local model", that small local models diagnose less well than large ones and may propose less useful commands, which the same rules check, so the worst outcome is an incident handed to a person sooner. Nothing discourages using one.

**Which AI handled an incident** is already in its history: every `context_sent`, `ai_proposal` and stop is recorded with the AI as actor, `provider/model`, now `claude/claude-opus-5-5`, `openai/gpt-5.1` or `openai-compatible/qwen3:8b`, inside the hash chain. The audit log shows it as "AI · openai-compatible/qwen3:8b".

**Safety stays on the server.** The loop codes against `dyn AiProvider` and never learns which provider answered. Tests run the real OpenAI adapter against a fake server through the real loop: a local model's `rm -rf --no-preserve-root /`, written as JSON text with "this is read-only" as its reasoning, is classified forbidden and escalates without anything queued; an OpenAI `apt-get install` claiming to be safe waits for an administrator even where low-impact commands run alone.

## Consequences

- Product's non-goal "a local or self-hosted AI model" is gone: an AI that keeps all data on the network is a choice on the page.
- Weak models cost more requests: one more for a malformed answer, one more the first time a server refuses tools. Neither repeats endlessly.
- The JSON-in-text fallback cannot tell a model's answer from prose that happens to hold one JSON object; whatever it holds is checked and classified like any proposal.
- The private-address check resolves names before each request and reqwest resolves them again; an attacker who controls the DNS of a name the administrator chose could still answer differently in between. HTTPS has no such gap.
- Switching providers means entering the new provider's key; switching back means entering the old one again.
- Gemini and others with their own APIs are reached through their OpenAI-compatible endpoints or LiteLLM; a native adapter is one more file when someone needs it.

## Amendment: the address and its key (2026-11-09)

- `GET /api/settings/ai` returns `base_url` to global administrators only, null to everyone else: every user reads whether AI is on, and the address of a local model is often inside the operator's network.
- The stored key of an `openai_compatible` server belongs to its stored address as well as to its provider. Saving, testing or listing models with another address and no pasted key is refused with "enter the API key again: the stored one was entered for another address". Before, the key followed any address typed in the form, also through Test and the model list, which save and audit nothing. To move to a server without a key, remove the key first. Anthropic and OpenAI have one address each, so nothing changes for them.
