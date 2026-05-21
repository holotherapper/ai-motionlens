# Internals - wait_policy, external_state, evidence, capabilities

These are details the SKILL.md hints at but does not spell out. Read this once and reference it when you hit a confusing field in a response.

## Trigger timing model

Every `trigger.*` records its `at_t_ms` at the **current** virtual time and
dispatches the event **without burning any virtual budget**. The event
listener's synchronous body runs immediately; any `requestAnimationFrame` /
transition / WAAPI animation scheduled by the handler only advances when you
call `clock.advance` or `frame.capture_series` next.

This means `target_times_ms: [0, 60, 150, ...]` after a `trigger.click` is
correct -- t=0 captures the first paint of the handler's synchronous effect,
and the later samples drive the animation forward.

## `wait_policy` on triggers

`trigger.click / hover / type / scroll / evaluate` all accept an optional `wait_policy`:

| Value | What it does | Use when |
|---|---|---|
| `none` (default) | Returns as soon as the JS handler dispatched | Most cases. The virtual clock is paused, so any subsequent `clock.advance` is your wait |
| `until-network-idle` | Waits for the CDP `Page.lifecycleEvent` of `networkAlmostIdle`, with a 1.5s soft timeout | Trigger fires a `fetch()` and you need the response before your next capture |
| `until-next-frame` | Resolves on the next `requestAnimationFrame` callback, with a 1.5s soft timeout | Trigger schedules a rAF callback whose effect you want to see at t=0 |

Both `until-*` policies hand off to the page's real event loop. The soft
timeout exists because the virtual clock is paused during the wait -- if the
page never schedules another frame or the network never quiets, the agent
must still be able to make progress.

In practice, `none` is correct ~95% of the time because animations are driven
by the virtual clock that you control with `clock.advance`.

## `external_state` (`session.capabilities.external_state`)

Each field is one of `live` / `recorded` / `pinned` / `disabled`:

| Field | Default | Determinism implication |
|---|---|---|
| network | live | Responses may differ across captures. Scratch replay will refetch |
| service_worker | live | SW may serve stale on retry |
| storage | live | localStorage / IndexedDB carries over within a session |
| web_socket | live | Real-time message ordering is not reproducible |
| random | live | `Math.random()` / `crypto.getRandomValues()` is real entropy |
| crypto | live | Same as above |
| timezone | live | Real system timezone |
| locale | live | Real `navigator.language` |
| user_agent | live | Default headless UA string |
| third_party_iframes | live | Different across captures |
| media_devices | disabled | Camera/mic locked off |

If an `intent_match` fails after a scratch replay, check whether the failure depends on a `live` field. A failure that depends on real network data is **not necessarily a bug in your animation code**.

## `evidence` - why it exists

The evidence ledger is the agent's persistent observation log within an episode. It accumulates:
- `frames[]` (every `capture` is appended)
- `triggers[]` (every `trigger.*` is recorded with virtual timestamp)
- `diffs[]` (`motion.diff` results)
- `observations[]` (your claims via `evidence.append_observation`)
- `intent` (from `episode.set_intent`)

Why bother:
1. **Iteration memory**: after `evidence.list` you can see "I already captured this interval" without re-doing it.
2. **Final report**: when the loop ends, the ledger is your handover document. Read it back and write the user-facing summary.
3. **Scratch replay needs it**: `frame.bisect` replays from `start_state` + `triggers[]`. If you bypass `trigger.*` and `dom_query`-evaluate directly, the bisect will not re-execute those side effects.

If your work is "one quick check then done", you can skip evidence. For multi-iteration animation work, append at least one observation per iteration so you can review what changed.

## `confidence` (`evidence.append_observation` field)

A number in `[0.0, 1.0]`. Suggested mapping:

| Value | Meaning |
|---|---|
| 0.95-1.0 | Both your vision AND the structured numbers agree; the claim is solid |
| 0.7-0.95 | Vision is clear, but numbers are ambiguous (or vice-versa) |
| 0.4-0.7 | One signal supports the claim, the other is silent |
| <0.4 | Best guess, more evidence needed |

`evidence.list` later shows the confidence so future iterations of the agent can prioritize re-verifying low-confidence claims.

## `can_seek_back` vs `live_clock_forward_only`

`session.capabilities` returns both. They mean different things:

| Field | Value | Meaning |
|---|---|---|
| `can_seek_back` | `true` | Some way to observe a past timestamp exists |
| `seek_back_strategy` | `replay-scratch` | The way is: open a fresh tab, replay |
| `live_clock_forward_only` | `true` | The currently-open tab cannot be rewound. `clock.advance` is forward-only |

So `frame.capture_series(target_times_ms: [500, 300])` errors out (ascending required). But `frame.bisect(interval: {t0: 100, t1: 300})` works because if `mid` is past, the server opens a scratch tab.

## `live_clock_forward_only` in practice

Plan ascending. If you genuinely need a past sample, the scratch-replay cost in `capabilities.replay_cost` tells you whether it's cheap or not:

```
replay_fixed_cost_ms      # page reload + setup
cost_per_virtual_ms       # cost to drive scratch clock 1 virtual ms
cost_per_trigger_ms       # cost to re-fire one recorded trigger
capture_cost_ms           # one screenshot
network_wait_risk         # 0..1, probability the page stalls on a network fetch
```

Estimated cost = `fixed + cost_per_virtual_ms * t_ms + cost_per_trigger_ms * trigger_count + capture_cost_ms`. If this is > a few hundred ms, prefer to recapture ascending instead of bisecting backwards.
