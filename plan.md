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
  `mouse`, `incsearch`, `hlsearch` (both on, `:noh`), status templates. `<C-\>` returns to normal from any mode. A broken config falls back to defaults and says why.
- **status bar**: vim-tabline tabs (current reversed, activity bold, bell yellow), slides around the current
  terminal when it overflows. Fields `{mode} {name} {index} {total} {modified} {workflow} {terms}`.
- **resume**: `vix -r` restores names, live cwd (pid -> cwd like tmux), and scrollback (`dump`), not programs.
- **workflows**: `:w name` saves the list to `~/.config/vix/workflows/name.toml`, after which `:w` keeps saving
  there. `vix name` opens one (or a new one, like `vim newfile`). Per terminal: `name`, `cwd` (`~` ok), `cmd`.

## next: prompt editing

Vim motions and operators on whatever prompt the program in view n is showing: a shell, a REPL, claude code's input.
Output above it stays read-only (yank still works). No snapshot, no shared buffer: the screen is the truth, vix edits
a copy of the input and sends the difference to the program as keys. This replaces the old snapshot idea.

### what was verified (drive, zsh and claude code 2.1)

- both keep the real cursor on their input; arrows move it. Left past the start clamps, so overshooting left is safe
- claude code wraps its input box itself (rows aren't vt-wrapped), which is why `I` on a second row lands on that
  row's start today. Its box is one string though: left arrows cross rows, and each break counts as one char (the
  space the wrap ate). A real newline (shift+enter) also counts as one, so arrow math holds either way

### how

1. **find the input**: the cursor's line as one string plus a map to screen cells. vt wraps join with zero chars,
   program wraps (continuation rows indented alike, stopping at a border or blank row) with one. The program's
   cursor is our index into it
2. **edit it with the existing vim**: in view n, operators whose range is inside the input are allowed; everywhere
   else stays read-only as today. `x dw ciw D p r ~`, visual, text objects as they land
3. **send the difference**: common prefix/suffix of old and new, then arrows to the change's end, backspaces over
   it, the new text, arrows to the new cursor. Rules: never move right past the input's end (right accepts fish and
   zsh autosuggestions), only backspace (forward delete isn't universal), insert as bracketed paste when the
   program enabled it, else refuse newlines (a raw newline runs the command), count graphemes
4. **the program redraws**, view n refreshes, and a wrong guess corrects itself. The prompt string (`$ `) inside the
   "input" is harmless: shells refuse to move or delete into it. `c` and `s` delete, then enter view i

### where it works

| kind | examples | |
|---|---|---|
| line editors | zsh, bash, fish, python, psql, node | solid. risks: autosuggestion ghost text, `RPROMPT` |
| main-screen input boxes | claude code (verified), likely codex, gemini | the continuation-row heuristic |
| full screen | nvim, less, htop | off (alternate screen); they have their own modes |
| no line editor | `cat`, `read` | arrows print junk; rare, accepted |

Programs with their own vi mode (claude code's `editorMode: vim`, zsh `bindkey -v`) break step 3 while they're in
their own normal mode: backspace and pasted text become commands. Detect it from the cursor shape (DECSCUSR, block
vs bar) if programs set it, else document "turn off the program's vi mode".

### steps

1. `prompt::keys(old, cursor, new, new_cursor, paste) -> bytes`: pure, tested alone
2. `prompt::input(screen) -> Input`: string + cell map, tested on screens captured with drive from zsh, bash, fish,
   python, claude code, codex. `I` / `A` use it, which fixes `I` on claude code's wrapped rows
3. view n wiring: operators inside the input, changes out through step 1
4. typing faster than the redraw (`xxxx`): base the next edit on the expected text until the screen catches up
5. with shell integration (server step 6): exact input start from OSC 133 `B`, ghost text by its color

Not worried about yet: undo across refreshes, trailing-space trimming in `vt::text`.

## the server

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

Newline-delimited JSON over the unix socket, bytes as base64. Debuggable with `socat`, easy for agents. The flood
measurement in 3g found no need for binary frames. The messages are in server.md's protocol section.

`ctl` adds connections that send requests and get replies without attaching: they aren't clients, get no output,
and don't take the pty size. Settling that handshake (`Hello { kind: ui|ctl }` or similar) is step 4's first part.

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
3. ~~**server/client**~~ (done): `vix --server`, attach via dump + stream, detach on hangup and `:qd`, `-a`, `-l`.
   Detailed in server.md.
4. **ctl**: ls, new, kill, rename, send, read, wait, apply, save.
5. **sessions are workflows**: `vix web` attaching or starting shipped in step 3. Left: `vix -d web` starts
   detached, and `:w name` renames the session it saves.
6. **shell integration**: OSC 133, `run`, `wait --prompt`, prompt jumps, running command in the list.

## vim scope

All valid text **motions** should exist in every buffer (list, view n, the prompt). Operators, registers, and ex
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
| `prompt` | pure | next: the input under the program's cursor; edits -> arrows, backspaces, paste |
| `follow` / `carry` | pure | view n following output; `i a I A` moving a line editor's cursor |
| `pty` / `proc` | io | spawn/resize/read/write, pid -> cwd |
| `vt` / `dump` | io-ish | `vt100` per pty, query replies, text extraction; screen + scrollback as replayable bytes |
| `render` / `input` | io | ratatui drawing; stdin incl. kitty `CSI u` and SGR mouse |
| `config` / `session` / `workflow` | io | config.toml, `--resume`, workflow files |
| `mux` | io | the running list: terminals, spawn / kill / reconcile, output -> flags |
| `protocol` | pure | the messages between client and server |
| `server` / `outbox` | io | `vix --server`: owns the `Mux`, serves clients, one bounded writer per client |
| `client` / `sock` | io | the remote list and vt replicas; socket paths, starting servers, `-l` |
| `ctl` | io | next: thin client for scripts and agents |

vix is a multiplexer, not an emulator: it runs inside ghostty/kitty/etc. like tmux, and keeps a vt parser per pty
only to know what's on screen.

## decided

- bare `vix` starts a new session; `vix name` attaches or starts the workflow's session
- every way of quitting vix (`:qa`, `:q` from the list) kills the session, as today. Detaching is its own command,
  `:qd` / `:detach`, plus hangup of the outer terminal
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

## concepts
agents or people can get a suite of commands that let you drive and read vix
vix read {workspace} {name} # read contents of a terminal
vix append {workspace} {name} {wd = .} {cmd = none} # create terminal
vix call {workspace} {name} {cmd} # run a command in a terminal
vix mv # concept to move a workspace to another computer
vix remote # concept to orchestrate remote terminals instead of driving it inside them, now you can orchestrate multiple remote sessions from one central computer
vix workflows # list workflows to update or delete 
vix kill {detached id}