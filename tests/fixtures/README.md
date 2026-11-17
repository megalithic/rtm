# Fixtures

- `rex.json`: the first two sessions from one read-only live run of
  `rex --autostart=false do src/rex/snapshot.lua` on 2026-11-17. Paths, labels, messages,
  users, host IDs, session/window/block/layer IDs and process IDs are replaced with fake
  values. Process ancestry and view/layer structure are retained. Timestamps are unchanged.
- `rex-empty.json`: a sanitized shell block projected from the same capture's empty-status
  shape. Rex encodes empty Lua lists as `{}`, including `status.records`.
- `ultimate.json`: synthetic instances/conversations, including the live-observed
  `parent: {conversationId, at}` schema, busy/recent/stale/concluded conversations and a question.
- `enrichment.json`: synthetic registry, worktrees, PR and seen timestamps.

The model tests deserialize these fixtures; controlled mutations exercise ordering,
folding, unread done, multiple agents and matching without live IO.
