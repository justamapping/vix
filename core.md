# Vix

Vix is a terminal multiplexer with the same philosophy as vi. It's a visual way to see and take action on your terminals while using 
vim like keybinds.

## philosophy

A terminal fits a specific archetype dimensionally speaking. We could think of it as many different things to start

1. A terminal is a 3d block in a 3d space which can be observed two dimensionally
2. A terminal is a 1d block in a 2d space. i.e. a character within a text editor relative to an entire file of lines
3. A terminal is a 2d row in a 2d space. i.e. a terminal *is* the line, we differentiate by either the incremental key (integer), or the title of the terminal

option 3 is the most practical when thinking of all the options. We observe the screen like option 1, and we could simulate this in a game (we have),
but practically speaking option 3 is the clear winner. So what does this mean?

It means that a list of terminals first and foremost starts visually across the entire screen. Not panes, not tabs, not windows.
It's an array of terminals displayed visually on the screen. That agency wise has been made clear from this thought experiment

We should make this as malleable as possible starting with keybinds.

## problem

Tmux is great groundwork for the idea that one terminal should control many. it's become one of the most important programs in todays age, yet
it feels like a path of subtle resistance. Vim solved this with text editing and gave many savvy users a fast way to navigate and edit text
in a world where it became increasingly important to. Many of the top programmers used vim to write and navigate their text.

Today things are clearly changing with AI, yet there remain many invariants. People need a way to access and manage terminals abstractly without
it being a burden or a complex state. 

Many are using and creating new "agent interfaces" under the guise of the agent interface being the core primitive

We've tried tabs, threads, panes, necklaces, blocks, and so on. I suspect that a good foundational ergonomic terminal multiplexer won't be the be-all solution to this,
but I feel as if it would scratch the itch many didn't know they had, similar to vi.

## criticism

trying to turn things into vim has failed many times aside from vim itself. This could be because it isn't made with the core understanding of why vim
works so well. Going :newchrometab<Enter> is hardly what vim is. I argue that nnn is one of the most successful vim-esque implementations
in a new domain, while being minimal, buggy, and non malleable.

this program could be argued as a small plugin to tmux, but I feel as if knowing tmux as prior art, while keeping this separate gives us less baggage
and room to innovate in this regard. whatever the innovation may be

this program could secretly exist already, but even if so, I think it's worth while to faithfully attempt to make the terminal multiplex experience
as malleable, and ergonomic as possible.

## requirements

A user should be able to:

- type commands similar to vim like :x while inside an actual terminal to exit without deletion, :q would quit the terminal and delete it

- search and access terminals ergonomically namely through "/" searching and finding the text when inside the orchestrator state

- recursively (if they choose) call this program. i.e. vix -> terminal, nvim, ai harness, vix -> ...

- name their program if they want to with vim style text editing

- change their own keybinds via a vix configuration file

- use this mutually recursively with tmux if they wish, i.e. tmux -> 1: vix -> tmux -> [], 2: vim

- use their main program without interference, including things like claude code, neovim, servers, and so on
