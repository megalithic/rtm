# rtm

A launcher for [Rex](https://www.superlogical.com/rex) that lists what is running — Ultimate instances and their
conversations, Rex sessions and the agents in them — puts what needs you first, previews the highlighted one, and
jumps there.

The data layer is implemented; the floating Rex layer, previews and keys below are planned UI work.

## Data layer

```sh
rtm snapshot --json --timing          # live Rex + Ultimate; cached registry and PRs
rtm snapshot --json --wait --timing   # also wait for cache refresh
rtm seen 'session:<id>'               # fast, atomic last-viewed update; no discovery
```

Rex is read through the embedded `src/rex/snapshot.lua` with autostart disabled. Ultimate
lockfiles are checked with signal 0; only the status, conversations and entries read actions
are used. No sessions are created, focused, killed or otherwise modified.

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

See `tests/fixtures/README.md` for fixture provenance. No ratatui or async runtime is needed.

## Keys (planned)

| Key | Does |
| --- | --- |
| type | search names first, then agent conversations and details |
| ↑ ↓, ctrl+p ctrl+n | move |
| ctrl+b ctrl+f | scroll the preview |
| enter | go to the session, or focus the Ultimate tab on that conversation |
| ctrl+o | open or focus Tidewave Connect for the row's Phoenix app |
| ctrl+t | new worktree (`rwt new`) from the row's repository, based on origin's default branch |
| ctrl+shift+t | new worktree based on the highlighted worktree's branch |
| ctrl+k | `kill <name>?`, then y: stop the session's processes (`rwt down`), keeping the worktree |
| esc | close |

## Install

```toml
# mise config
"cargo:https://github.com/megalithic/rtm" = "branch:main"
```

## License

MIT
