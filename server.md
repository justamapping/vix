# Detach: the server and the client

Step 3 of plan.md. The goal is that nothing about using vix changes, except that you can leave and come back.

## in plain terms

Today vix is one process. It owns the shells and draws them. When it exits, the shells die.

After this step it's two processes:

- the **server** owns the shells. It has no screen and keeps running in the background.
- the **client** is what you see. It draws, handles keys, and owns the list buffer and view n, as today.

They talk over a socket file. Detaching means the client exits and the server keeps going. Attaching means a new
client connects, gets a copy of every terminal's screen, and carries on from there.

```
 outer terminal (ghostty)                         background
┌──────────────────────────┐   socket file    ┌──────────────────────┐
│ client: vix              │ ◄──────────────► │ server: vix --server │
│  keys, list, view n,     │  keys, :w, :e →  │  ptys + shells       │
│  drawing, a screen copy  │  ← output, list  │  the list (Mux)      │
│  per terminal            │                  │  the real screens    │
└──────────────────────────┘                  └──────────────────────┘
```

## resume vs reattach

These are two features, and both stay.

- **resume** (`-r`, workflows) is residue: names, directories, and the text programs left behind, with fresh shells.
  An nvim or claude code comes back as leftover text, which is expected.
- **reattach** is live: the same processes, still running, with their exact screens.

Why reattach works at all: a program talks to a pty, not to your terminal. Whoever holds the pty's other end keeps
it alive. Today that's vix, so quitting vix closes it and the program gets SIGHUP. After this step the server holds
it, and the program never notices a client coming or going.

Direction, not decided yet: one implicit rule for `vix web`. If the session is running, attach to it. Otherwise
start it from the workflow, with residue if there is any.

## what you see

| you do | what happens |
|---|---|
| `vix` | starts a new session (named `0`, `1`, ...) and attaches. Looks exactly like today |
| `vix web` | attaches to session `web` if it's running, else starts it from the workflow |
| `vix -r` | starts a new session from the last one you quit, as today |
| `:qd` (also `:detach`) | detaches. Shells keep running. Prints `[detached from web]` like tmux |
| closing the window, ssh dropping | detaches |
| `:qa`, `:q` from the list | kills the session, as today, for every attached client |
| `vix -a [name]` | attaches to `name`, or the most recently detached session |
| `vix -l` | lists sessions: name, terminal count, whether attached |
| vix crashes | the shells survive. `vix -a` gets them back |

On attach you land where you left: the terminal you were looking at, in view i. Staged list edits live in the
client, so `:qd` with a modified list fails with E37 unless you use `:qd!`, like `:q`.

## who owns what

| server | client |
|---|---|
| `Mux`: ptys, the real vt parsers, ids, names, cwd, cmd | a screen copy (replica) per terminal |
| answering programs' queries (cursor position etc.) | the list buffer, staged edits, view n, the picker |
| activity/bell flags, quiet periods | config: keymaps, status bar, escape key |
| `:w` reconcile, `:e` spawns, kills, renames | which terminal you're viewing, alternate, mode |
| carry (`i a I A`), since it needs the pty's raw mode | rendering, outer terminal modes, OSC 52 yank |
| workflow save, the `--resume` snapshot on quit | the `:qd` / `:qa` decision |

The rule: anything that touches a process or must agree across clients lives on the server, and anything about
your screen lives on the client.

## decisions

**The client keeps a full copy of every screen** (flagged: unconventional). tmux and zellij render in the server
and send drawn cells. Here, the server forwards raw program output and the client parses it into its own `vt100`
copy. That keeps view n, search, scrollback, and drawing local and instant, and `app.rs` keeps reading
`t.vt.screen()` like today. The cost is that both sides must parse identically, which a version check on attach
guarantees, since they're the same binary.

**Attach needs an exact screen copy.** `dump::dump` is built for `--resume`. It flattens the main screen and drops
the cursor, the alternate screen, and modes, so nvim or claude code would come back wrong. A new `dump::exact` is
needed, built from:

- every scrollback and screen row at full width, so the replica's scrollback comes out exactly
- vt100's `state_formatted()` for the cursor and modes
- if a full-screen program is running, `?1049h` followed by its screen

Its test is a round trip: fill a parser, replay `exact` into a fresh one, and assert they match.

**No request/reply waits.** The client never blocks on the server. Things that need a result, like `:e logs` then
opening it, or `<CR>` on a modified list then opening that row, send the action with an "open" hint. The server
answers with the new list plus `open: id`, in order on the same stream, and the client opens it when it arrives.
Errors come back as a message for the bar.

**One code path.** There's no in-process mode. Plain `vix` starts a server too, so there's one path to test. `Mux`
moves behind the server, and its unit test stays.

**One server per session.** This is like zellij, not tmux. There's no session layer to build, a crash takes down
one session, and a session is one workflow. The server is the same binary: `vix --server <name>`, started with
`setsid` and stdio pointing at a log file.

**The server lives as long as it has terminals or clients.** When the last terminal exits and nobody is attached,
it exits and removes its socket. While attached, an empty list stays open, as today.

**The protocol is newline-delimited JSON, with bytes in base64** (`serde_json`, a new dependency). It's slower than
tmux's binary messages, but you can read it with `socat` and agents can use it later. Swap only if a flood test
says so.

**The terminal size follows the most recently active client.** That's tmux's `window-size latest`. The server
resizes the ptys and sends `Resized` in-stream, so every replica resizes at the same byte. A client whose window is
a different size draws clipped or padded.

**Flags follow focus.** Each client tells the server what it's viewing. A terminal counts as viewed if any client
is viewing it, so it isn't flagged. Focusing a terminal clears its flags for everyone.

**Sockets.**

- path: `$XDG_RUNTIME_DIR/vix/<name>.sock`, else `$TMPDIR/vix-$UID/<name>.sock`, in a 0700 directory
- if connecting is refused, the socket is stale: remove it
- macOS caps socket paths at 104 bytes, so long names error early

**Environment.** Every shell gets `VIX_SOCKET` and `VIX_ID`, which `ctl` needs later and the next point needs now.
Shells inherit the environment of the client that started the server. Later attaches don't update it.
tmux's `update-environment` handles that, if it's ever needed.

**Nesting.** Plain `vix` inside vix nests a new session, as before. `vix -a` targeting the session you're inside
refuses, because it would mirror itself forever.

## protocol

```rust
enum Request {                                     // client -> server
    Hello { version: u32, cols: u16, rows: u16, start: Option<Start> },
    Input { id: u64, bytes: Vec<u8> },
    Carry { id: u64, at: Pos },                    // i a I A on a line editor
    Resize { cols: u16, rows: u16 },
    Focus { id: Option<u64> },
    Write { base: Vec<u64>, lines: Vec<Line>, open: Option<usize> },   // :w, <CR> on a modified list
    Spawn { name: String, from: Option<u64> },     // :e / :new, opens it
    Kill { id: u64 },                              // :q!, :d
    Rename { id: u64, name: String },              // :r
    Save { workflow: Option<String> },             // :w name, or :w with a workflow
    Refresh,                                       // fresh cwds, for the picker
    Info,                                          // vix -l, without attaching
    Detach,
    KillSession,                                   // :qa
}

enum Reply {                                       // server -> client
    Attached { cols: u16, rows: u16, list: List, screens: Vec<(u64, Vec<u8>)>, focus: Option<u64> },
    Output { id: u64, bytes: Vec<u8> },
    List(List),                                    // after any change; `open` only to the client that asked
    Flags { id: u64, activity: bool, bell: bool },
    Resized { cols: u16, rows: u16 },
    Exited { id: u64 },                            // "logs exited", then a List
    Message { text: String },                      // errors, "web" 3 terminals written
    Info { name: String, terms: usize, clients: usize },
    Bye { reason: String },                        // killed, version mismatch
}

struct List { terms: Vec<Entry>, workflow: Option<String>, open: Option<u64> }
struct Entry { id: u64, name: String, cwd: PathBuf, cmd: Option<String>, activity: bool, bell: bool }
struct Start { session: Session, workflow: Option<String> }   // what a new session spawns
```

### flows

```
attach     C: Hello{start?}     S: (spawns start if new) Attached{screens = exact dumps}, then live Output
type       C: Input             S: pty write ... Output (to every client)
:w         C: Write{base}       S: reconcile (3-way diff from step 2), List to all
<CR>+:w    C: Write{open: row}  S: List{open: id} to the asker, who opens it
:e logs    C: Spawn             S: List{open: id} to the asker
exit       -                    S: Exited{id}, List to all. Clients rebase their staged list (step 2)
:qd        C: Detach            S: drops the client. The client restores the terminal and prints [detached]
:qa        C: KillSession       S: snapshot for -r, kill all, Bye to all, remove the socket, exit
```

The server handles everything in one loop over one event channel, as `main.rs` does today. Attach is one
iteration (dump everything, register the client), so no output can slip between the dump and the stream.

### threads

These are plain threads and `mpsc`, as today, with no async runtime.

- **server**:
  - the main loop owns `Mux`
  - an accept thread
  - a reader thread per client, which sends events into the loop
  - a writer thread per client, which drains a queue to the socket
  - the existing pty pumps
- **client**: stdin and signals as today, plus a socket reader thread, which sends `Reply`s into the same loop.
  Writes go straight to the socket.

## how App changes

`App` holds a `client::Remote` instead of a `Mux`.

`Remote` holds:
- the replicas: `client::Term { id, name, cwd, vt, activity, bell }`
- the socket

Reads stay the same shape (`terms()`, `get`, `position`, `len`). Each write becomes a message:

| today | after |
|---|---|
| `t.write(bytes)` | `Input` |
| `self.carry` (needs `raw()`) | `Carry` |
| `t.seen()` in `set_view` | `Focus` |
| `self.write()` returning ids | `Write{open}`; opens on `List{open}` |
| `self.edit` spawning | `Spawn`; opens on `List{open}` |
| `remove` + `kill` | `Kill`, then the list rebases when `List` arrives |
| `rename` | `Rename` |
| `save_workflow`, `snapshot` | `Save`, and `KillSession` (the server snapshots) |
| `Event::Output` -> `t.feed` | `Reply::Output` -> `replica.process`, replies and bell dropped |
| `Event::Resize` -> `mux.resize` | `Resize`; replicas resize on `Resized` |
| picker cwd from `t.cwd()` | `Entry.cwd`, refreshed when the picker opens |

## build steps

Each one is testable on its own. 3a to 3c don't touch the UI.

Tagged the current commit `pre-server` (done) so the single-process vix is always one build away.
Once servers exist, the harness must also kill the session on drop, since killing the client leaves it running.

| step | what | test |
|---|---|---|
| 3-0 | (done, `tests/ui.rs`) regression suite: today's `drive.rs` flows (`:w` ops, `:e :r :d`, exits under edits, picker, workflows, `-r`, bar overflow) recorded as expected screens against single-process vix | must still pass after 3e: userspace unchanged |
| 3a | `dump::exact`: scrollback, screens, alternate screen, cursor, modes | round trip into a fresh parser, cell for cell |
| 3b | `protocol.rs`: the enums, NDJSON read/write | encode/decode round trip, base64 bytes |
| 3c | `sock.rs`: socket dir, session names, stale sockets, `-l` info, starting the server | temp dir: naming, stale removal |
| 3d | `server.rs`: `vix --server`, accept, the loop over `Mux`, every `Request` | a test client on a temp socket: Hello, type `echo hi`, read it back, Kill, KillSession |
| 3e | `client.rs` + `App` on `Remote`, `main.rs` start-or-attach, `:qd`, hangup = detach | drive.rs: type, `:qd`, `vix -a`, same screen |
| 3f | `-a`, `-l`, the nesting guard, version check, `-r` through the server, `VIX_SOCKET`/`VIX_ID` | drive.rs |
| 3g | slow clients: bounded queue, on overflow drop and resend screens. Size follows latest | flood a terminal while a client is stopped |

3e is the big one. It's mostly mechanical, following the table above. After 3f it's shippable; 3g is hardening.

## risks

- **Screen fidelity.** If `exact` misses something, a reattached TUI looks wrong until it redraws. Mitigations:
  - the round-trip test
  - after attach, nudging each pty with a resize to the same size, which makes most TUIs redraw (tmux does
    this too)
- **Output floods.** `yes` in one terminal pushes a lot of JSON. That's measured in 3g, and the server already
  batches.
- **Upgrading vix while a session runs.** The old server and new client mismatch. The version check refuses with
  "session web runs an older vix, kill it or use that binary". tmux has the same problem.
- **Server crashes.** The server has no screen, so it logs to `$XDG_STATE_HOME/vix/<name>.log`. When the socket
  closes unexpectedly, the client restores the terminal and prints `[server exited, see <log>]`. The shells
  surviving a client crash is a strict improvement on today.
- **Key latency.** It adds one local socket hop each way, which is microseconds. Worth confirming by feel.

## not in this step

- `vix ctl` (step 4, on this same protocol)
- renaming a session when `:w name` names its workflow (step 5)
- OSC 133 (step 6)
