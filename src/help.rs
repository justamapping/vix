use crate::config::{self, Config};
use crate::input::Key;
use crate::keyspec;
use crate::remap::Map;

/// The keys of a sheet row, shown through the user's maps.
enum Keys {
    /// key sequences in vim notation
    Bind(&'static [&'static str]),
    /// ex commands, whose `:` goes through the maps
    Ex(&'static [&'static str]),
    /// the configured escape key
    Escape,
    /// shown as written
    Text(&'static str),
}

use Keys::*;

#[derive(Clone, Copy)]
enum Maps {
    List,
    View,
}

struct Group {
    title: &'static str,
    maps: Maps,
    /// rows read `what: keys`, for rows whose keys run long
    label_first: bool,
    rows: &'static [(Keys, &'static str)],
}

const GROUPS: &[Group] = &[
    Group {
        title: "between terminals",
        maps: Maps::View,
        label_first: false,
        rows: &[
            (Text("1-9"), "go to terminal n, 9 is the last"),
            (Bind(&["<Tab>"]), "next terminal that rang, else with new output"),
            (Bind(&["J", "K"]), "next / previous terminal, counts work (3J)"),
            (Bind(&["<C-^>"]), "alternate terminal"),
            (Bind(&["<C-p>"]), "find a terminal by name or directory"),
            (Bind(&["-"]), "up to the list"),
        ],
    },
    Group {
        title: "in a terminal",
        maps: Maps::View,
        label_first: false,
        rows: &[
            (Escape, "stop typing, to view n; twice sends it to the program"),
            (Bind(&["i", "a", "I", "A"]), "type, moving the prompt's cursor there"),
            (Bind(&["o", "O"]), "type, leaving the prompt's cursor alone"),
            (Bind(&["v", "V", "y"]), "select and yank to the clipboard"),
            (Bind(&["/", "?", "n", "N"]), "search the output"),
            (Ex(&[":noh"]), "hide search highlights"),
            (Bind(&["x", "dw", "ciw", "p"]), "operators on the prompt edit the program's input"),
            (Ex(&[":r name"]), "rename it"),
            (Ex(&[":d", ":d name"]), "kill it (or name) and move to a neighbor"),
        ],
    },
    Group {
        title: "the list",
        maps: Maps::List,
        label_first: false,
        rows: &[
            (Bind(&["<CR>"]), "open the terminal under the cursor"),
            (Text("edit lines"), "rename, dd kills, yy p clones, o adds, dd p reorders"),
            (Ex(&[":w"]), "apply the edits"),
            (Ex(&[":w name"]), "save as a workflow, reopened with vix name"),
            (Ex(&[":e name", ":new name"]), "view or spawn a terminal by name, from anywhere"),
        ],
    },
    Group {
        title: "leaving",
        maps: Maps::View,
        label_first: false,
        rows: &[
            (Ex(&[":q"]), "view n: up to the list. the list: quit, killing the session"),
            (Ex(&[":q!"]), "view n: kill the terminal"),
            (Ex(&[":qd"]), "detach, the session keeps running (vix -a)"),
            (Ex(&[":qa"]), "quit and kill the session"),
        ],
    },
    Group {
        title: "vim",
        maps: Maps::View,
        label_first: true,
        rows: &[
            (Bind(&["h", "j", "k", "l", "w", "b", "e", "W", "B", "E", "0", "^", "$", "gg", "G"]), "motions"),
            (Bind(&["f", "t", "F", "T", ";", ","]), "find"),
            (Bind(&["<C-e>", "<C-y>", "<C-d>", "<C-u>", "<C-f>", "<C-b>", "H", "M", "L"]), "scrolling"),
            (Bind(&["d", "y", "c", "x", "X", "D", "C", "s", "S", "Y", "r", "~", "p", "P", "u", "<C-r>"]), "edits"),
            (Text("iw aw iW aW ip ap i\" i' i` i( i[ i{ i<"), "text objects"),
        ],
    },
];

/// The narrowest first column; wider when a group's cells need it.
const WIDTH: usize = 16;

/// `:h` and `vix --keys`: the default keys as the user's config reaches them, then the user's own maps.
pub fn sheet(config: &Config) -> Vec<String> {
    let groups: Vec<(&str, Vec<(String, String)>)> = GROUPS
        .iter()
        .map(|g| {
            let maps = match g.maps {
                Maps::List => &config.list,
                Maps::View => &config.view,
            };
            let rows = g.rows.iter().map(|(keys, what)| match (shown(keys, maps, config.escape), g.label_first) {
                (keys, true) => (format!("{what}:"), keys),
                (keys, false) => (keys, what.to_string()),
            });
            (g.title, rows.collect())
        })
        .collect();
    let firsts = groups.iter().flat_map(|(_, rows)| rows.iter().map(|(first, _)| first.chars().count() + 2));
    let width = firsts.max().unwrap_or(0).max(WIDTH);
    let mut out = vec!["vix keys, as your config has them. q closes, / searches".to_string()];
    for (title, rows) in groups {
        out.push(String::new());
        out.push(title.into());
        out.extend(rows.into_iter().map(|(first, second)| format!("  {first:<width$}{second}")));
    }
    let defaults = config::default_view();
    let own = [
        ("list", &config.list),
        ("list insert", &config.list_insert),
        ("view n", &config.view),
    ];
    for (mode, maps) in own {
        let maps: Vec<_> = maps.iter().filter(|m| !defaults.contains(m)).collect();
        if maps.is_empty() {
            continue;
        }
        out.push(String::new());
        out.push(format!("your maps ({mode})"));
        for m in maps {
            out.push(format!("  {:<width$}{}", keyspec::notation(&m.lhs), keyspec::notation(&m.rhs)));
        }
    }
    out
}

fn shown(keys: &Keys, maps: &[Map], escape: u8) -> String {
    let seqs = |s: &[&str]| s.iter().map(|s| keyspec::parse(s).expect("sheet keys parse")).collect::<Vec<_>>();
    match keys {
        Bind(s) => seqs(s).iter().flat_map(|k| reach(k, maps)).map(|k| keyspec::notation(&k)).collect::<Vec<_>>().join(" "),
        // `:e name, :new name`, since ex commands have spaces of their own
        Ex(s) => s
            .iter()
            .flat_map(|cmd| reach(&[Key::Char(':')], maps).into_iter().map(move |k| format!("{}{}", keyspec::notation(&k), &cmd[1..])))
            .collect::<Vec<_>>()
            .join(", "),
        Escape => keyspec::ctrl_key(escape).map_or_else(|| format!("{escape:#04x}"), |k| keyspec::notation(&[k])),
        Text(t) => t.to_string(),
    }
}

/// The keys that run `seq` under non-recursive `maps`: itself unless a mapping takes it over, then every
/// mapping whose right side is exactly it.
fn reach(seq: &[Key], maps: &[Map]) -> Vec<Vec<Key>> {
    let taken = maps.iter().any(|m| seq.starts_with(&m.lhs));
    let own = (!taken).then(|| seq.to_vec());
    own.into_iter().chain(maps.iter().filter(|m| m.rhs == seq).map(|m| m.lhs.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::{self, Parsed};

    fn map(lhs: &str, rhs: &str) -> Map {
        Map { lhs: keyspec::parse(lhs).unwrap(), rhs: keyspec::parse(rhs).unwrap() }
    }

    /// The row mentioning `what`, with its columns one space apart.
    fn row(sheet: &[String], what: &str) -> String {
        let line = sheet.iter().find(|l| l.contains(what)).unwrap_or_else(|| panic!("no row for {what}"));
        line.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn every_listed_key_does_something() {
        for g in GROUPS {
            for (keys, _) in g.rows {
                let Bind(seqs) = keys else { continue };
                for s in *seqs {
                    assert_ne!(keymap::parse(&keyspec::parse(s).unwrap()), Parsed::Invalid, "{s} in {}", g.title);
                }
            }
        }
    }

    #[test]
    fn defaults() {
        let sheet = sheet(&Config::default());
        assert_eq!(row(&sheet, "up to the list"), "- q up to the list");
        assert_eq!(row(&sheet, "spawn a terminal"), ":e name, :new name view or spawn a terminal by name, from anywhere");
        assert!(row(&sheet, "stop typing").starts_with("<C-\\>"));
        assert!(row(&sheet, "detach").starts_with(":qd"));
        assert!(!sheet.iter().any(|l| l.starts_with("your maps")));
    }

    #[test]
    fn follows_the_users_maps() {
        let mut config = Config { escape: 0, ..Config::default() };
        config.view.extend([map(";", ":"), map(":", ";"), map("<C-k>", "<C-u>"), map("q", ":q<CR>")]);
        config.view.retain(|m| m != &map("q", "-"));
        config.list_insert = vec![map("jk", "<Esc>")];
        let sheet = sheet(&config);
        assert!(row(&sheet, "detach").starts_with(";qd "));
        assert!(row(&sheet, "scrolling").contains("<C-u> <C-k>"));
        assert!(row(&sheet, "up to the list").starts_with("- "));
        assert!(row(&sheet, "stop typing").starts_with("<C-Space>"));
        let own = sheet.iter().position(|l| l == "your maps (view n)").unwrap();
        let own: Vec<_> = sheet[own + 1..].iter().map(|l| l.split_whitespace().collect::<Vec<_>>().join(" ")).collect();
        assert_eq!(own, ["; :", ": ;", "<C-k> <C-u>", "q :q<CR>"]);
        assert!(sheet.contains(&"your maps (list insert)".to_string()));
    }

    #[test]
    fn shadowed_keys_drop_out() {
        assert_eq!(reach(&[Key::Char('g'), Key::Char('g')], &[map("g", "x")]), Vec::<Vec<Key>>::new());
        assert_eq!(reach(&[Key::Char('-')], &[map("q", "-")]), [vec![Key::Char('-')], vec![Key::Char('q')]]);
    }
}
