# Vix plan

See core.md for the philosophy. This is the working plan: what's built, what's next, and what's still open.
Nothing here is final; build the pure pieces, use it, see what sticks.

## model

A terminal is a line. Everything vix does is either editing that list of lines, or looking at one line unfolded.

State is two axes: **view × mode**.

| | normal | insert | visual | command |
|---|---|---|---|---|
| **list** (outer) | motions/operators over terminal names | edit names as text | select terminals | `:w`, `:q`, ... |
| **view** (inner) | motions over the terminal's text | keys go to the program | select text | `:x`, `:q`, ... |

`i` always means "insert into what you're looking at": the name in list, the program in view.

### transitions

```
vix                  list n      one terminal, "Untitled"
list n   --<CR>-->   view i      open terminal under cursor, start typing
view i   --<C-\>-->  view n      the only key vix steals (configurable)
view n   --<C-\>-->  view i      only right after entering, and sends a literal <C-\>
view n   --i/a-->    view i      (i a I A carry the line editor's cursor; o O just enter)
view n   -- - / q -> list n      also :q, :x
view n   --:q!-->    list n      kills the terminal and drops its line
view n   --:d [n]--> view n      kills (named) terminal, moves to alternate/neighbor like :bd
view n   --J/K-->    view n      next/prev terminal, counts work (3J)
view n   --1-9-->    view n      chrome's cmd+1-9, 9 is last
view n   --<C-^>-->  view n      alternate terminal
any      --<C-p>-->  picker      fuzzy over name + cwd, previews the selection
any      --:qa-->    quit vix    (:q only quits from the list)
```

## built

- **list**: names with hidden stable ids; `:w` diffs into kill / rename / clone / spawn / reorder (`listdiff`).
  `<CR>` on a modified list asks first. `:e name`, `:new name` from anywhere.
- **view n**: read-only buffer over scrollback + screen, follows live output at the bottom, pinned elsewhere.
  Motions, visual, search, yank via OSC 52, `:r name`, `:d [name]`. Mouse: wheel scrolls into view n, drag selects.
- **config**: `~/.config/vix/config.toml`, vim key notation, non-recursive maps per mode, `escape`, `timeoutlen`,
  `mouse`, status templates. A broken config falls back to defaults and says why.
- **status bar**: vim-tabline tabs (current reversed, activity bold, bell yellow), slides around the current
  terminal when it overflows. Fields `{mode} {name} {index} {total} {modified} {workflow} {terms}`.
- **resume**: `vix -r` restores names, live cwd (pid -> cwd like tmux), and scrollback (`dump`), not programs.
- **workflows**: `:w name` saves the list to `~/.config/vix/workflows/name.toml`, after which `:w` keeps saving
  there. `vix name` opens one (or a new one, like `vim newfile`). Per terminal: `name`, `cwd` (`~` ok), `cmd`.

## next: the server

Detach, `vix ctl`, agents driving terminals, and running workflows headless all look like separate features. They
aren't: they all fall out of making the list a long-lived object that more than one thing can talk to.

### the core primitive

The list, with three verbs on it:

| verb | what | already exists as |
|---|---|---|
| **reconcile** | desired list -> ops (spawn, kill, rename, clone, reorder) | `listdiff::diff`, `:w` |
| **io** | send keys to a terminal, read its text | pty write, `vt::text` |
| **watch** | stream of output, flags, exits, list changes | `Event` in `app.rs` |

Everything else is a composition:

- the **UI** is a client that watches everything, renders, and turns local buffer edits into reconcile and io
- a **ctl** call is one reconcile / io / watch, then exit
- an **agent** is a ctl caller, often running inside one of the terminals it controls
- **detach** is the UI leaving while the list keeps running
- a **workflow** is a saved desired list; a **session** is a running one. `vix web` attaches to the running list
  named web, else starts it from the saved one. `:w` saves the running list back. File <-> buffer <-> process, like vim

So the server owns one list, and there is no separate session/window/pane hierarchy to learn.

### reconcile is a 3-way diff

With several writers (two UIs, an agent), a client's `:w` must not kill what it never saw. The client sends the
ids it based its edits on: `diff(base, current, edited)`. Ids in `base` and missing from `edited` die; ids the server
has that weren't in `base` (spawned by someone else meanwhile) stay. On a list change a client rebases its staged
buffer with `Buffer::rebase(current)`, which exits, `:e`, and `:r` already go through.

Interactive edits match by id. Applying a workflow file matches by name (first unused match), so
`vix ctl apply web` is idempotent: run it twice, nothing happens the second time. `--prune` kills extras.

### process model

- one server per session (like zellij, unlike tmux's one-server-many-sessions): no session layer in the server,
  crash isolation, and a session is exactly one workflow
- socket `$XDG_RUNTIME_DIR/vix/<session>.sock`, else `$TMPDIR/vix-$UID/<session>.sock`, dir 0700
- one binary: `vix` connects, starting `vix --server <session>` (setsid, stdio to /dev/null) if nothing listens
- the server exits when its last terminal exits or on `kill-session`; SIGHUP of the UI is a detach, not a kill
- every terminal gets `VIX_SOCKET` and `VIX_ID`, so `vix ctl` inside a terminal targets its own session and itself
  by default. Plain `vix` inside vix still nests a new session (core.md's recursion requirement)

### what lives where

| server | client (UI) |
|---|---|
| ptys, vt parsers (canonical; answers DA/DSR) | list buffer and its staged edits |
| the list: ids, names, cwd, cmd, order | view n buffer, view × mode, picker |
| activity/bell flags, cleared when a client focuses | vt replicas, fed by dump + output stream |
| workflow name, scrollback | config, keymaps, outer terminal modes, rendering |

The client keeps its own vt replica per terminal rather than asking the server for screens: rendering, view n, and
search stay local and instant, and `app.rs` barely changes (a `Pty` becomes a remote handle). Attach is
`dump::dump` per terminal (already written for `--resume`), then raw output. Clients discard their replicas' query
replies; only the server answers.

Pty size follows the most recently active client (tmux's `window-size latest`).

### protocol

Newline-delimited JSON over the unix socket, bytes as base64. Debuggable with `socat`, easy for agents; swap
for binary frames only if a flood benchmark says so.

```
client -> server                         server -> client
Hello { kind: ui|ctl, cols, rows }       Attached { list, dumps, flags }
Input { id, bytes }                      Output { id, bytes }
Resize { cols, rows }                    List { terms }            (on any change)
Focus { id }                             Flags { id, activity, bell }
Write { base, lines }                    Exited { id, status }
Spawn / Kill / Rename / Save             Reply { ok | error, ... }
Read { id, range } / Wait { id, until }
```

### ctl

`vix ctl <verb>`, a thin client. A target `<t>` is a name, an index (`3`), `%id`, or `.` (self; the default inside vix).

```
vix ctl ls [--json]                      index, id, name, state, cwd, cmd
vix ctl new [name] [-c dir] [-- cmd]     prints the id
vix ctl kill <t> | rename <t> <name>
vix ctl send <t> 'npm test<CR>'          vim key notation (keyspec); --raw for literal bytes
vix ctl read <t> [--all | --lines n]     the text view n would show
vix ctl wait <t> --match pat | --idle 2s | --exit | --prompt   [--timeout]
vix ctl run <t> 'npm test'               send + wait for the prompt, print that command's output, exit with its status
vix ctl apply <workflow|file> [--prune] | save <workflow>
vix ctl sessions | detach | kill-session
```

`ctl` also becomes the test harness: scripts that `send` and `read` replace `examples/drive.rs`.

### shell integration

`run` and `wait --prompt` need to know where a command's output starts and ends and how it exited. That's OSC 133
(prompt / command / output / exit marks), which fish emits natively and which vix can inject for zsh/bash the way
ghostty, kitty, and vscode do (`ZDOTDIR`, `--rcfile`). Without it, fall back to `--idle`.

The same marks give humans `[[` `]]` between prompts in view n, "yank last output", and the running command and
exit code for the list and the bar (old milestone 5).

### steps

Each one ships on its own.

1. ~~**mux**~~ (done): move terminal ownership out of `App` into `mux.rs` (list, spawn, kill, reconcile, io, flags, events).
   `App` uses it in-process. No behavior change.
2. ~~**3-way diff**~~ (done) in `listdiff`, and `Buffer::rebase` for the staged list on list changes.
3. **server/client**: `vix --server`, attach via dump + stream, detach on hangup and `:qd`. Detailed in server.md.
4. **ctl**: ls, new, kill, rename, send, read, wait, apply, save.
5. **sessions are workflows**: `vix web` attaches or starts, `vix -d web` starts detached.
6. **shell integration**: OSC 133, `run`, `wait --prompt`, prompt jumps, running command in the list.

## view n editing (open)

The whole terminal output should be editable, not just navigable. Editing can't affect the process, so editing
means a snapshot. Current lean: an edit operator (`d c x p`) freezes view n into a snapshot (`[snapshot]` in the
bar). While frozen, `i a` edit text instead of going to the program; `:e!` returns to live. `:w file`, `:w !cmd`.
Needs text objects (`iw iW i" ip`), `% { } * # .`, `<C-v>` first; those matter for yanking from output anyway.

## vim scope

All valid text **motions** should exist in every buffer (list, view n, snapshot). Operators, registers, and ex
commands grow from a small set. Homemade for now; `nvim --embed` behind the same interface if it gets uncanny.

## architecture

Rust. Pure modules first, I/O at the edges.

| module | kind | does |
|---|---|---|
| `state` | pure | view transitions on the escape key, `1`-`9` goto |
| `keymap` | pure | key sequences, counts, pending operators -> actions |
| `motion` | pure | text motions over `&[String]` + cursor |
| `listdiff` | pure | `:w` rules |
| `buffer` | pure | the small vim over lines with hidden ids |
| `remap` / `keyspec` | pure | non-recursive mappings, vim key notation |
| `status` | pure | bar templates, overflow |
| `picker` | pure | fuzzy finder over terminals |
| `follow` / `carry` | pure | view n following output; `i a I A` moving a line editor's cursor |
| `pty` / `proc` | io | spawn/resize/read/write, pid -> cwd |
| `vt` / `dump` | io-ish | `vt100` per pty, query replies, text extraction; screen + scrollback as replayable bytes |
| `render` / `input` | io | ratatui drawing; stdin incl. kitty `CSI u` and SGR mouse |
| `config` / `session` / `workflow` | io | config.toml, `--resume`, workflow files |
| `mux` | io | the running list: terminals, spawn / kill / reconcile, output -> flags |
| `server` / `client` / `ctl` | io | next: socket, attach, thin ctl client |

vix is a multiplexer, not an emulator: it runs inside ghostty/kitty/etc. like tmux, and keeps a vt parser per pty
only to know what's on screen.

## decided

- bare `vix` starts a new session; `vix name` attaches or starts the workflow's session
- every way of quitting vix (`:qa`, `:q` from the list) kills the session, as today. Detaching is its own command
  (name open: `:qd`? `:xd`?), plus hangup of the outer terminal
- `vix ctl <verb>`, kept compartmentalized until its use is clearer

## open questions


- `read` for agents: rendered text (what a human sees, works for TUIs) vs an escape-stripped output log with
  offsets for incremental reads (works for streams, garbage for TUIs). lean: rendered, with OSC 133 ranges
- show who spawned a terminal (agent vs you) in the list?
- workflow `cmd` capture: read the foreground process (`tcgetpgrp` + argv) on `:w`; type it on load without running it?
- clone semantics beyond "same cwd" (re-run the foreground command?)
- vt parser: `vt100` until fidelity breaks (claude code, nvim), then `alacritty_terminal`
- `/` from the list: names only, or scrollback across all terminals into the picker?
- restrict names so the rest of the line is free for annotations (`web  # dev server`)?
