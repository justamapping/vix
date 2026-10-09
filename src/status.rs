use anyhow::{Context, Result, bail};

/// One terminal as the bar sees it.
#[derive(Debug, Clone, Default)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub current: bool,
    pub alternate: bool,
    pub activity: bool,
    pub bell: bool,
}

#[derive(Debug, Default)]
pub struct Info<'a> {
    pub mode: &'a str,
    pub modified: bool,
    pub terms: Vec<Entry<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    Mode,
    Name,
    Index,
    Total,
    Modified,
    Terms,
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    Field(Field),
}

/// A bar template like `"{mode}"` or `"{modified} {terms}"`.
#[derive(Debug, Clone, PartialEq)]
pub struct Template(Vec<Piece>);

impl Template {
    pub fn parse(s: &str) -> Result<Self> {
        let mut pieces = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            let end = start + rest[start..].find('}').with_context(|| format!("unclosed {{ in {s:?}"))?;
            if start > 0 {
                pieces.push(Piece::Text(rest[..start].into()));
            }
            let field = match &rest[start + 1..end] {
                "mode" => Field::Mode,
                "name" => Field::Name,
                "index" => Field::Index,
                "total" => Field::Total,
                "modified" => Field::Modified,
                "terms" => Field::Terms,
                other => bail!("unknown status field {{{other}}}"),
            };
            pieces.push(Piece::Field(field));
            rest = &rest[end + 1..];
        }
        if !rest.is_empty() {
            pieces.push(Piece::Text(rest.into()));
        }
        Ok(Self(pieces))
    }

    pub fn expand(&self, info: &Info) -> String {
        let current = info.terms.iter().position(|t| t.current);
        let mut out = String::new();
        for piece in &self.0 {
            match piece {
                Piece::Text(t) => out.push_str(t),
                Piece::Field(Field::Mode) => out.push_str(info.mode),
                Piece::Field(Field::Name) => out.push_str(current.map_or("", |i| info.terms[i].name)),
                Piece::Field(Field::Index) => out.push_str(&current.map_or(String::new(), |i| (i + 1).to_string())),
                Piece::Field(Field::Total) => out.push_str(&info.terms.len().to_string()),
                Piece::Field(Field::Modified) => out.push_str(if info.modified { "[+]" } else { "" }),
                Piece::Field(Field::Terms) => out.push_str(&terms(&info.terms)),
            }
        }
        out.trim().to_string()
    }
}

/// Names with tmux's flags: `*` current, `-` alternate, `#` activity, `!` bell.
fn terms(entries: &[Entry]) -> String {
    let flagged = entries.iter().map(|t| {
        let mut s = t.name.to_string();
        for (on, flag) in [(t.current, '*'), (t.alternate, '-'), (t.activity, '#'), (t.bell, '!')] {
            if on {
                s.push(flag);
            }
        }
        s
    });
    flagged.collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> Entry<'_> {
        Entry { name, ..Default::default() }
    }

    fn info() -> Info<'static> {
        Info {
            mode: "-- TERMINAL --",
            modified: false,
            terms: vec![
                Entry { current: true, ..entry("web") },
                Entry { alternate: true, ..entry("api") },
                Entry { activity: true, bell: true, ..entry("claude") },
            ],
        }
    }

    #[test]
    fn fields() {
        let t = |s: &str| Template::parse(s).unwrap().expand(&info());
        assert_eq!(t("{mode}"), "-- TERMINAL --");
        assert_eq!(t("{index}/{total} {name}"), "1/3 web");
        assert_eq!(t("{terms}"), "web* api- claude#!");
        assert_eq!(t("[{name}]"), "[web]");
    }

    #[test]
    fn empty_fields_trim() {
        let list = Info { mode: "", terms: vec![entry("web")], ..Default::default() };
        assert_eq!(Template::parse("{modified} {terms}").unwrap().expand(&list), "web");
        assert_eq!(Template::parse("{index} {name}").unwrap().expand(&list), "");
        let modified = Info { modified: true, ..list };
        assert_eq!(Template::parse("{modified} {terms}").unwrap().expand(&modified), "[+] web");
    }

    #[test]
    fn bad_templates() {
        assert!(Template::parse("{nope}").is_err());
        assert!(Template::parse("{mode").is_err());
        assert!(Template::parse("").unwrap().0.is_empty());
    }
}
