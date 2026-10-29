# ashlar-assistant

An in-app AI assistant harness that does not depend on one model or vendor: the app supplies
tools and a UI, the harness runs the conversation.

## Pieces

| | |
|---|---|
| `types` | `Conversation` (append-only), `Message` / `Block` (text, thinking, tool call, tool result), `Native` (a provider's own content, replayed verbatim), `ToolSpec`, `StreamEvent`, `Usage`, `price(model)` |
| `Provider` | one streamed request: `send(&Request, events, &Cancel) -> Response` |
| `AccountBackend` | an app's own backend that holds the provider key (`GET /v1/ai/status`, `POST /v1/ai/messages`, Anthropic wire format, bearer token from a `TokenSource`): one retry with a refreshed token on 401; a 429 whose `Retry-After` is at most 15 s is waited out and retried once (a longer one, such as a daily limit, is reported with the wait); the server's `x-request-id` is added to errors for support; content blocks it adds (`fallback`) and usage fields it adds (`iterations`) pass through and are replayed verbatim |
| `AnthropicApi` | `api.anthropic.com/v1/messages` with the user's key (`x-api-key`, `anthropic-version: 2026-10-21`, refusal fallbacks on the models that take them) |
| `OpenAiCompatible` | `{base}/chat/completions` (OpenAI, OpenRouter, Ollama, LM Studio…), key optional; tool calls mapped both ways |
| `run_turn` | the agent loop over the app's `ToolHost` and `Observer`; the tool list is read once per turn |
| `CombinedTools` | several tool hosts as one (the app's tools, then integrations'), calls routed by name |
| `spawn_turn` | the loop on a thread for a UI: `TurnHandle::poll / answer / cancel` |
| `Settings`, `build_provider`, `check_connection` | the saved choice of provider and model; a settings page's Test button (model list or AI status, no tokens spent) |
| `keys` | `KeyStore`: `KeychainKeys` (feature `keychain`, the OS keychain) or `MemoryKeys`; `ApiKey` never prints |
| `mcp::McpTools` | feature `mcp`: an app's `ashlar-mcp` server as the tool host, in process |
| `MockProvider` | scripted replies for tests |
| `testing` | feature `testing`: a loopback HTTP server and Anthropic-format streams |

Features: `http` (default; the three HTTP providers, desktop only), `keychain`, `mcp`, `testing`.
Without `http` (and on wasm32) the types, loop, settings and mock remain.

## Requests

Anthropic-format requests follow the current model defaults: always streamed; `thinking` either
omitted or `{"type": "adaptive", "display": "summarized"}`, never a budget; effort in
`output_config.effort` (`AgentConfig::new` uses `medium`); no `temperature` / `top_p` / `top_k`;
no prefill; `tool_choice: auto`; every tool with `eager_input_streaming`; the system prompt as one
block with `cache_control: ephemeral` (keep it and the tool order stable for caching). Each
assistant turn goes back exactly as it came (thinking blocks with signatures, redacted thinking,
fallback markers), and the conversation is never edited. Thinking blocks are bound to the prefix
that produced them (system prompt, tools, earlier messages): each reply records a fingerprint of
that prefix (`Native::binding`), and a later request replays a turn's thinking only while the same
prefix still has that fingerprint. A reopened chat whose system prompt or tools changed (a new app
version), or whose saved results were cut, leaves the stale reasoning out instead of being
refused; a refusal the client did not foresee drops all earlier reasoning and asks once more
(`Conversation::forget_thinking`).

## The loop

1. Send the conversation; stream text, thinking and tool-call arguments to the observer.
2. Append the reply. `max_tokens` or `refusal`: stop without running its calls (each gets an
   error result so the conversation stays valid). `pause_turn`: send again.
3. For each call: arguments that were not valid JSON, an unknown tool or arguments that fail the
   tool's schema get an error result; a call the host marks (`ToolHost::needs_approval`) waits
   for `Observer::approve`; then `ToolHost::call` runs it. Results over 100 000 bytes are cut.
4. All results go back in one user message; repeat until `end_turn`, `AgentConfig::max_steps`,
   an error or cancel. `TurnOutcome` has the summed usage and cost.

Errors (`Error`) display as sentences for the user: sign in, the plan does not include AI, the
budget is used up, rate limited (with the wait from `Retry-After`), the key was refused, no key,
offline, the provider failed, the conversation is too long.

## Using it

```rust,ignore
let provider = build_provider(&settings, &keys, Some(&account))?;
let mut conversation = old_conversation.clone();
conversation.push_user_text(text);
let handle = spawn_turn(provider, move || Box::new(my_tools()), conversation, AgentConfig::new(model, SYSTEM), wake);
// each frame: for e in handle.poll() { … }  ·  handle.answer(call_id, Approval::Allow)  ·  handle.cancel()
```
