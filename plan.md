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
view n   --<Tab>-->  view n      next terminal that rang, else with unseen output (also from the list)
any      --<C-p>-->  picker      fuzzy over name + cwd, previews the selection
any      --:qa-->    quit vix    (:q only quits from the list)
```

## built

- **list**: names with hidden stable ids; `:w` diffs into kill / rename / clone / spawn / reorder (`listdiff`).
  `<CR>` on a modified list asks first. `:e name`, `:new name` from anywhere. Each row shows its directory as
  virtual text (`cwds`): the fewest trailing components that tell it apart, hidden when it's the name.
- **view n**: read-only buffer over scrollback + screen, follows live output at the bottom, pinned elsewhere.
  Motions, visual, search, yank via OSC 52, `:r name`, `:d [name]`. Mouse: wheel scrolls into view n, drag selects.
- **config**: `~/.config/vix/config.toml`, vim key notation, non-recursive maps per mode, `escape`, `timeoutlen`,
  `mouse`, `incsearch`, `hlsearch` (both on, `:noh`), status templates. `<C-\>` returns to normal from any mode. A broken config falls back to defaults and says why.
- **status bar**: vim-tabline tabs (current reversed, activity bold, bell yellow; OSC 9/777/99 notifications ring), slides around the current
  terminal when it overflows. Fields `{mode} {name} {index} {total} {modified} {workflow} {terms}`.
- **resume**: `vix -r` restores names, live cwd (pid -> cwd like tmux), and scrollback (`dump`), not programs.
- **workflows**: `:w name` saves the list to `~/.config/vix/workflows/name.toml`, after which `:w` keeps saving
  there. `vix name` opens one (or a new one, like `vim newfile`). Per terminal: `name`, `cwd` (`~` ok), `cmd`.
- **prompt editing**: operators in view n edit the program's input (shells, REPLs, claude code), see below.
- **text objects**: `iw aw iW aW ip ap`, quotes `" ' \``, brackets `( [ { <` (and `b` `B`) with counts, after
  operators and in visual; `r`, `~`. Quotes pair up from the line's start (vim guesses from the cursor), brackets
  don't search forward. Not yet: sentences, tags, visual `r`/`~`, extending a selection with another object.
- **`vix ctl`**: ls, new, kill, rename, send, read, wait, apply, save, sessions, detach, kill-session. See "ctl".

## priorities

1. ~~**`vix ctl`**~~ (server step 4, done)
2. ~~**text objects**, `r`, `~`~~ (done, `textobj.rs`; list, view n and prompt editing share them)
3. ~~**fast edits**~~ (prompt editing step 4, done)
4. **shell integration**, only if its payoff is worth how opinionated it is (see "shell integration")

## next: prompt editing

Vim motions and operators on whatever prompt the program in view n is showing: a shell, a REPL, claude code's input.
Output above it stays read-only (yank still works). No snapshot, no shared buffer: the screen is the truth, vix edits
a copy of the input and sends the difference to the program as keys. This replaces the old snapshot idea.

### what was verified (drive, zsh and claude code 2.1)

- both keep the real cursor on their input; arrows move it. Left past the start clamps in zsh, but overshooting is
  not safe in general: claude code binds left on an empty prompt to its agent view (`← for agents` while a turn or
  background task runs). Today's `i` on the `❯` column sends exactly that. Arrows must stop at the input's start,
  so `I`/`i`/`a` go through `prompt::input` too, not raw screen distance
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
   zsh autosuggestions), only backspace (forward delete isn't universal), type text as keystrokes, bracketed paste
   only for newlines and tabs (refused if the program hasn't enabled it), count chars like the buffer does
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

1. ~~`prompt::keys`~~ (done): `(old, cursor, new, new_cursor, modes) -> bytes`, pure, tested alone
2. ~~`prompt::input(screen) -> Input`~~ (done): string + cell map, tested on screens in `tests/screens` captured with
   `DRIVE_CMD`/`DRIVE_SAVE` from zsh, bash, python 3.14, node, claude code (codex and fish not yet). The prompt ends
   at a glyph + blank inside a styled prefix, else at that prefix's end (prompts without a glyph), else at the first
   glyph + blank. Dim or bright-black text from the cursor on is ghost text. `i a I A` go through it and never pass
   the input's ends; `I`/`A` are the input's start and end, across claude code's wrapped rows
3. ~~view n wiring~~ (done): view n's buffer holds the input as a `prompt::Field` (view n coordinates). Operators
   and `p`/`P` whose cursor (and visual anchor) are on it run on a one-line copy of it, sharing view n's register, so
   yanked output pastes in. The client sends a `prompt::Change` (old text and cursor, new text and cursor); the
   server types it only if its own screen still shows that input and the cursor isn't a block (the program's own vi
   normal mode), else says why. `c` and `s` end in view i. `r`, `~`, text objects and `.` wait on the vim itself
4. ~~typing faster than the redraw~~ (done) (`xxxx`): each edit is computed from the last screen. If the server's screen moved
   on, the edit is refused (lost keystroke); if neither has but the program took the last edit, the keys assume the
   wrong cursor (wrong edit). Remember the text and cursor an edit should produce and base the next edit on it until
   the screen shows it or a short timeout passes. Most likely in claude code, which redraws slower than zsh; test a
   burst there first. Built that way: view n draws the predicted input into its line (when it's one row) and moves
   the cursor there, so motions and the next edit see it; the server accepts a change built on what it last typed.
   Both trust the prediction until the screen shows it or 500ms (`prompt::SETTLE`) pass. Bursts of `x ~ r dw P` and
   motions between them match vim in zsh, bash and claude code (drive, all keys in one write). Inputs wrapped over
   rows are predicted as if extra text continues the last row
5. ~~text objects, `r`, `~`~~: in the vim itself, nothing prompt-specific (done)

OSC 133 would only give shells an exact input start (`B`); REPLs and claude code don't emit it and nothing marks the
input's end, so the heuristics stay either way. Not a step here; it comes with shell integration if that happens.

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

`ctl` connections send requests and get replies without attaching: they never say `Hello`, so they get no
output and don't take the pty size. No new handshake: `Info` carries the protocol version, which ctl checks first,
and ctl sends another `Info` after each request as a barrier, so it knows the request was handled and sees its
`Error`s (split from `Message` so ctl can exit nonzero).

### ctl

`vix ctl <verb>`, a thin client (`src/ctl.rs`). A target `<t>` is a name, an index (`3`), `%id`, or `.` (the
terminal ctl runs in). Targets are required positionals, so `.` is explicit rather than a default. A name two
terminals share is an error, not the first match. The session is `-s`, else the one ctl runs in, else the only one
running; several is an error rather than a guess.

```
vix ctl ls [--json]                      index, id, name, state, cwd, cmd, foreground program
vix ctl new [name] [-c dir] [-- cmd]     prints the id
vix ctl kill <t> | rename <t> <name>
vix ctl send <t> 'npm test<CR>'          vim key notation (keyspec); --raw for literal bytes
vix ctl read <t> [--all | --lines n]     the text view n would show; --cursor, or --vt for replayable bytes
vix ctl wait <t> --done | --match pat | --idle 2s | --exit   [--timeout]
vix ctl run <t> 'npm test'               (needs shell integration) send + wait for the prompt, output, exit status
vix ctl apply <workflow|file> [--prune] | save <workflow>
vix ctl sessions | detach | kill-session
vix ctl kill-all                         every session, then stray vix servers (sockets gone), its own session last
```

Built as written, except `run`, which waits on shell integration. `wait` polls `read` every
100ms: `--done` is the shell back in the foreground (the pty's foreground process group is the shell, like tmux's
`pane_current_command`) with the screen still for 300ms, so a command just sent has time to start; it never fires
while a full-screen program or REPL is in front. `--match` is a regex over the screen (multi-line, so `^$` work per row), `--idle` is the screen unchanged
that long, `--exit` is the id gone from the list (a `%id` already gone counts, ids aren't reused). `new` defaults
to ctl's own directory and prints the `%id`. `apply` matches by name, appends what's missing, `--prune` kills the
rest; it doesn't reorder. `kill-session` and `kill-all` skip the version check, so they reach servers from older builds.

`scripts/drive` is the dev harness on top of it (it replaced `examples/drive.rs`): a scratch session from `vix -d -x -y`,
vix typed into it, then `send`, `wait --idle`, `read --cursor` per step, and `read --vt` for `tests/screens`. The
vix it drives keeps its sessions in a temp `XDG_RUNTIME_DIR`, killed on exit, so none outlive it.

### shell integration

`run` and `wait --prompt` need to know where a command's output starts and ends and how it exited. That's OSC 133
(prompt / command / output / exit marks), which fish emits natively and which vix can inject for zsh/bash the way
ghostty, kitty, and vscode do (`ZDOTDIR`, `--rcfile`). Without it, fall back to `--idle`.

The same marks give humans `[[` `]]` between prompts in view n, "yank last output", and the running command and
exit code for the list and the bar (old milestone 5).

Deferred until something needs it. Injecting it means vix changes how the user's shell starts (a vix-owned
`ZDOTDIR` whose `.zshrc` sources the real one, then adds hooks), so it needs an off switch; the less opinionated
option is a line users `source` themselves. `ctl wait --idle` and the prompt heuristics already cover most of it.
Revisit if `ctl run` (exact output, exit status) turns out to matter, then weigh payoff against opinionation.

### steps

Each one ships on its own.

1. ~~**mux**~~ (done): move terminal ownership out of `App` into `mux.rs` (list, spawn, kill, reconcile, io, flags, events).
   `App` uses it in-process. No behavior change.
2. ~~**3-way diff**~~ (done) in `listdiff`, and `Buffer::rebase` for the staged list on list changes.
3. ~~**server/client**~~ (done): `vix --server`, attach via dump + stream, detach on hangup and `:qd`, `-a`, `-l`.
   Detailed in server.md.
4. ~~**ctl**~~ (done): ls, new, kill, rename, send, read, wait, apply, save, sessions, detach, kill-session.
5. **sessions are workflows**: `vix web` attaching or starting shipped in step 3, `vix -d web` starts it detached
   (`-x`/`-y` size it). Left: `:w name` renames the session it saves.
6. **shell integration** (deferred, see above): OSC 133, `run`, `wait --prompt`, prompt jumps, running command in
   the list.

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
| `ctl` | io | `vix ctl`: thin client for scripts and agents |

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

### answers
- read --full shows full output, read --diff shows diffed. we can decide the default

- showing whether an agent spawned a terminal is probably a good idea, but we need a good solution for this opposed to one that's more of a bandaid. terminals spawned via ctl is an easier tell

- a canonical search like jetbrains does to find function definitions couldn't hurt (i think it's shift+shift), to search across all terminals and jump somewhere. would be super powerful

## concepts
agents or people can get a suite of commands that let you drive and read vix
vix read {workspace} {name} # read contents of a terminal
vix append {workspace} {name} {wd = .} {cmd = none} # create terminal
vix call {workspace} {name} {cmd} # run a command in a terminal
vix mv # concept to move a workspace to another computer
vix remote # concept to orchestrate remote terminals instead of driving it inside them, now you can orchestrate multiple remote sessions from one central computer
vix workflows # list workflows to update or delete 
vix kill {detached id}

a terminating character a user can't go past that shows things like a directory, i.e.
myterminal \trm ~/programs/.dotfiles

jetbrains idea-esque shift+shift search inside to search across all terminal output and show options to jump to