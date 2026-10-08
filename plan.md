# Vix plan

See core.md for the philosophy. This is the working plan: what to build, in what order, and what's still open.
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
list n   --<CR>-->   view i      open terminal under cursor, start typing, i.e. go into view<i>
view i   --<C-\>-->  view n      the only key vix steals; <C-\><C-\> sends a literal. our <Esc> key of vim
view n   --i/a-->    view i
view n   -- - -->    list n      (oil's "go up")
view n   --J/K-->    view n      next/prev terminal, counts work (3J)
view n   --<C-^>-->  view n      alternate terminal
```

## list view

The buffer is just names, one per line:

```
foo
bar
buzz
```
rmk: we can also open it under a configuration of these names/terminals, i.e. vix webworkflow

`jj<CR>` opens `buzz`. Edits are staged; `:w` applies, `u` / `:e!` discards.

### ids

Each line gets a hidden id when the buffer loads: its line position. The id travels with the line through edits
(yank/paste copies it). After `:w` ids are renumbered. Good enough to start; revisit if it breaks.

### `:w` rules

A pure function: `(old lines with ids, new lines with ids) -> Vec<Op>`.

| after `:w` | op |
|---|---|
| id missing | kill |
| id present, text changed | rename |
| id appears n times | clone n-1 times (new shell, same cwd + env) |
| line with no id | spawn fresh shell with that name |
| order changed | reorder |

So `yyp`, `Vjy` + `p`, `dd`, `ddp`, `cw`, `o` all fall out of plain text editing.

### unwritten changes

Behave like vim's `hidden`: `<CR>` into a terminal with a modified buffer is allowed, status shows `[+]`.
`<CR>` on a line with no id (not yet written) errors: "not written".

## view mode

Insert: full passthrough to the program except `<C-\>`.

Normal: the terminal's scrollback + screen becomes a text buffer. Scrolling and copying fall out.
- `hjkl w b e f t / ? n % v V <C-v> y` etc. as text motions
- `H`/`L` keep vim meaning (screen top/bottom)
- `J`/`K` change terminal (unused in a read-only buffer, so free)
- cursor on last line follows live output; anywhere else it's pinned
- alt-screen programs (nvim, htop) have no scrollback, so the buffer is just the screen

### editable output (open)

The whole terminal output should be editable, not just navigable. Editing can't affect the process, so editing
means working on a snapshot: delete noise, reshape, yank, `:w file`.

- **A: blended into view n.** Operators (`d c x`) work directly; the first edit freezes the buffer off live output.
  Problem: `i` already means "type into the program", so insert-mode edits need another way in.
- **B: separate text state.** A key (TBD) snapshots the output into its own editable buffer with full vim keys.
  Cleaner modes, one more state.

Leaning B for clarity; decide after view n exists.

rmk: it also means we get full access to the editing terminal output, somewhat unprecedented

## vim scope

All valid text **motions** should exist in every buffer (list, view n, editable output). Operators, registers,
and ex commands can grow from a small set. The text engine sits behind one interface so we can swap it later:

```rust
trait TextBuffer {
    fn load(&mut self, lines: Vec<Line>);
    fn key(&mut self, key: Key) -> Vec<Effect>;
    fn lines(&self) -> &[Line];
}
```

Start homemade. If it gets uncanny, try `nvim --embed` behind the same trait.

## architecture

Rust. Pure modules first, I/O at the edges.

| module | kind | does |
|---|---|---|
| `state` | pure | `(State, Key) -> (State, Vec<Effect>)`, the view × mode machine |
| `keymap` | pure | key sequences, counts, pending operators -> actions |
| `motion` | pure | text motions over `&[String]` + cursor |
| `listdiff` | pure | `:w` rules |
| `buffer` | pure | `TextBuffer` impl |
| `pty` | io | spawn/resize/read/write (`portable-pty`) |
| `vt` | io-ish | per-pty screen + scrollback state, parsed from the pty's escape sequences (`vt100` crate) |
| `render` | io | draw list / view / status line (`crossterm`, maybe `ratatui`) |
| `input` | io | parse stdin incl. kitty keyboard `CSI u` encodings of `<C-\>` |
| `config` | io | `~/.config/vix/config.toml` keybinds |
| `server` | io | later: daemon owns ptys, clients over unix socket |

Status line is one row: `3/7 buzz  NORMAL [+]`. ptys are sized to `rows - 1`.

### multiplexer, not emulator

vix runs inside the user's terminal (ghostty, kitty, ...), like tmux. It still needs a vt parser per pty
(tmux has its own in grid.c/screen.c): switching back to a terminal, view n text, `/` search, and list previews all
need to know what's on screen, which only exists as a stream of escape sequences. We use a crate for this, we don't
write one. Raw passthrough + SIGWINCH redraw (the dtach trick) works without it, but only for TUIs that redraw
themselves; a plain shell comes back blank.

## milestones

1. **spike**: one pty, raw passthrough, `<C-\>` drops to a placeholder screen and back. Proves input handling,
   the riskiest part. No vt yet; returning uses the SIGWINCH redraw trick.
2. **state machine + list**: `state`, `keymap`, `listdiff` with tests. Multiple terminals, `j k gg G <CR> o dd yy p
   cw i <Esc> :w u`, redraw from emulator on switch.
3. **view n**: scrollback as text buffer, motions, `J/K`, `<C-^>`, `/`, yank to clipboard (OSC 52).
4. **status**: per-terminal running command, cwd (OSC 7), bell, exited; maybe as virtual text in list.
5. **config**: keybinds from toml.
6. **server/client**: persistence, detach/attach, `$VIX` nesting depth, shell integration (`:x`/`:q` functions
   calling `vix ctl` over `$VIX_SOCKET`).
7. **editable output**: option A or B.

## open questions

- editable output: blended or separate state, and the key to enter it
- clone semantics beyond "same cwd + env" (re-run foreground command?)
- vt parser: start with `vt100`, move to `alacritty_terminal` if fidelity breaks (claude code, nvim)
- homemade vim vs `nvim --embed` once the list and view n exist
- `:x` vs `:q` from inside a shell: `:q` = back to list (vim never destroys on `:q`), `:q!` = kill?
- `/` from list: search names only, or scrollback too (jump to terminal + match line)?

## answers
- blended state for now
- start with cwd, later try current process
- it's not a terminal emulator, it's comparable to tmux, it's a multiplexer
- start with homemade for now
- q! could delete it, x or q could save it buffer wise (keeps its name and process)
