# rtm

A launcher for [Rex](https://www.superlogical.com/rex) that lists what is running — Ultimate instances and their
conversations, Rex sessions and the agents in them — puts what needs you first, previews the highlighted one, and
jumps there.

rtm runs in a floating Rex layer opened by a key binding (see `rex/rtm.lua`).

## Keys

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
