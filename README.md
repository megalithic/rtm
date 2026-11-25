# rtm

A launcher for [Rex](https://www.superlogical.com/rex) that lists what is running — Ultimate instances and their
conversations, Rex sessions and the agents in them — puts what needs you first, previews the highlighted one, and
jumps there.

```sh
rtm pick [<client_id> [<block_id>]]   # the launcher; Rex opens it in a floating layer
rtm kill <session_id> [<client_id>]   # a small "kill <name>?" prompt; y runs rwt down
```

`rtm pick` draws its first frame from the last model cached in `~/.cache/rtm/model.json`
(about 1 ms), then refreshes live in the background. Typing searches. The highlighted row's
preview is the bottom of its Rex block (`rex capture --format vt`) or the last 40 entries of
its Ultimate conversation, refreshed while highlighted. Jumps focus the Rex block, or switch
the Ultimate instance's tabs to the conversation and raise its Helium tab over CDP
(`PIDEWAVE_CDP_PORT`, default 9223). `--bench-first-frame` prints the first-frame time;
`--keys "ctrl+n ctrl+k"` presses keys before the first frame.

## Data layer

```sh
rtm snapshot --json --timing          # live Rex + Ultimate; cached registry and PRs
rtm snapshot --json --wait --timing   # also wait for cache refresh
rtm seen 'session:<id>'               # fast, atomic last-viewed update; no discovery
rtm needs [--json]                    # what waits on you, per hub: this Mac's instances and `remotes`
rtm automation <url> <id> <name> pause|resume|run [--json]   # change one hub automation
```

`rtm needs` counts each hub's Inbox exactly as the picker's Inbox row does (open questions to
the user plus open Inbox items), for this Mac's running instances and every other host's URL in
`remotes` of `~/.ultimate/settings.json`. A hub that doesn't answer (1 s local, 3 s remote) has
`count: null` and its `error`, never 0; `total` sums the hubs that answered. The menu bar app
and Rex trackers read it.

Each hub also lists its `automations` (pinned first, then worst first), each with `pinned`,
`enabled`, `kind` and `state` in the words of Ultimate's Automations panel (failed, watching,
scheduled, armed, paused, done…). `rtm automation` pauses, resumes or runs one by id, and
refuses when the hub's automation with that id is no longer called `<name>`: ids restart in
every Ultimate chapter, so the hub checks it again (`expectName`) in the change's own
transaction. It only reaches a hub `rtm needs` reads, and only loopback `http://127.0.0.1:<port>`
or tailnet `https://*.ts.net` URLs.

Rex is read through the embedded `src/rex/snapshot.lua` with autostart disabled. Ultimate
lockfiles are checked with signal 0; only the status, conversations and entries read actions
are used, plus `automations/list` and, from `rtm automation`, its pause, resume and run. Snapshots never create, focus, kill or otherwise modify sessions; only `rtm pick` actions do.

Registry and PR refreshes use a background subprocess so they complete even after the snapshot
command exits. Registry refreshes each time; PRs refresh after 60 seconds. The default cache is
`~/.cache/rtm/{registry.json,prs/<stable-repo-hash>.json}`. `refresh.json` records refresh timings
and errors. Failed refreshes preserve the previous cache. Missing caches give no enrichment
until refreshed; `--wait` is useful on first launch. `XDG_CACHE_HOME` overrides the cache base.

Seen timestamps live in `~/.local/state/rtm/seen.json` (`XDG_STATE_HOME` overrides the state base).
`seen::Store` exposes `mark(key)` and `get(key)`; keys are `rex:<session id>`,
`ult:<project>#<conversation id>`, and `ult:<project>`. Writes merge under a sibling advisory
lock and atomically rename a temporary file. `seen` does not load any other sources.

Library modules:

- `rex`, `ultimate`: typed live snapshots, host locations, hub discovery; `ultimate::entries`
  fetches the newest 40 preview entries on demand, without reversing their order.
- `registry`, `git`, `cache`, `seen`: enrichment, PR colours, persistent state.
- `model`: pure `build(Inputs)` projection into Ultimate/Rex groups, rows and optional children.
- `search`: pure nucleo search returning parent/child positions and field-specific Unicode
  highlight indices. Nonempty searches rank globally by tier, viewed time, attention, score;
  an empty query preserves the grouped model order.
- `snapshot`: parallel source loading, background refresh orchestration and source timings.

Source failures appear in snapshot `warnings` or instance `metadata.errors`, not as fabricated
empty-success responses. Status only identifies the *first* conversation with a question;
if several questions exist, the instance is marked but only that known conversation can be
marked individually. Only ordinary repositories whose common directory is `<main>/.git`
are supported; other layouts are reported instead of guessing a main checkout.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

See `tests/fixtures/README.md` for fixture provenance.

## Keys

| Key | Does |
| --- | --- |
| type | search names first, then agent conversations and details |
| ↑ ↓, ctrl+p ctrl+n | move |
| ctrl+b ctrl+f | scroll the preview |
| enter | go to the session, or focus the Ultimate tab on that conversation |
| tab | Shade's tell mode on the row's project Ultimate: `--url` for an instance and its rows, `--project <main checkout>` for a Rex session |
| ctrl+o | open or focus Tidewave Connect for the row's Phoenix app |
| ctrl+t | new worktree (`rwt new`) from the row's repository, based on origin's default branch; the name starts as the search in kebab case. Rex sessions and project headers only |
| ctrl+shift+t | new worktree based on the highlighted row's branch (read from git for Ultimate rows) |
| ctrl+k | `kill <name>?`, then y: stop the session's processes (`rwt down`), keeping the worktree |
| esc | clear the search, then close |

Clicking a PR number opens it in the browser.

## Install

```toml
# mise config
"cargo:https://github.com/megalithic/rtm" = "branch:main"
```

## License

MIT
