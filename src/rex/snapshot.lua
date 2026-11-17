-- One snapshot of every Rex session: its terminal blocks, the process in each and the
-- status its program reports (OSC 7501). Run with `rex do`; prints JSON on stdout.
-- A script sees another session's blocks only after attaching to it.
local T = "com.superlogical.terminal"
local out = {}
local sessions = rex.call("session.list")
for _, s in ipairs(sessions and sessions.sessions or {}) do
  local item = { id = s.session_id, label = s.label, blocks = {} }
  rex.call("session.attach", { session_id = s.session_id })
  local view = rex.call("session.view", { session_id = s.session_id })
  local blocks, err = rex.call("session.list_blocks", { session_id = s.session_id })
  if not blocks then item.error = tostring(err) end
  item.view = view
  for _, b in ipairs(blocks and blocks.blocks or {}) do
    if b.creator_name == T then
      local function method(name)
        return rex.call("block.method", { session_id = s.session_id, block_id = b.block_id, method = name })
      end
      table.insert(item.blocks, {
        id = b.block_id,
        label = b.label,
        process = method("process"),
        status = method("program_status"),
      })
    end
  end
  table.insert(out, item)
end
print(rex.json.encode(out))
