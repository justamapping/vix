use anyhow::{Context, Result, bail};
use unicode_width::UnicodeWidthStr;

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
    pub workflow: &'a str,
    pub terms: Vec<Entry<'a>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Field {
    Mode,
    Name,
    Index,
    Total,
    Modified,
    Workflow,
    Terms,
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    Field(Field),
}

/// How a piece of the bar is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Text,
    Mode,
    Current,
    Alternate,
    Other,
    Activity,
    Bell,
    /// `<` `>` where terminals didn't fit
    More,
}

impl Kind {
    fn is_term(self) -> bool {
        matches!(self, Kind::Current | Kind::Alternate | Kind::Other | Kind::Activity | Kind::Bell)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Seg {
    pub text: String,
    pub kind: Kind,
}

fn seg(text: impl Into<String>, kind: Kind) -> Seg {
    Seg { text: text.into(), kind }
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
                "workflow" => Field::Workflow,
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

    pub fn expand(&self, info: &Info) -> Vec<Seg> {
        let current = info.terms.iter().position(|t| t.current);
        let mut out = Vec::new();
        for piece in &self.0 {
            let text = |s: String| seg(s, Kind::Text);
            match piece {
                Piece::Text(t) => out.push(text(t.clone())),
                Piece::Field(Field::Mode) => out.push(seg(info.mode, Kind::Mode)),
                Piece::Field(Field::Name) => out.push(text(current.map_or("", |i| info.terms[i].name).into())),
                Piece::Field(Field::Index) => out.push(text(current.map_or(String::new(), |i| (i + 1).to_string()))),
                Piece::Field(Field::Total) => out.push(text(info.terms.len().to_string())),
                Piece::Field(Field::Modified) => out.push(text(if info.modified { "[+]" } else { "" }.into())),
                Piece::Field(Field::Workflow) => out.push(text(info.workflow.into())),
                Piece::Field(Field::Terms) => out.extend(terms(&info.terms)),
            }
        }
        trim(out)
    }
}

/// One tab per terminal, like vim's tabline: ` 1 web `.
fn terms(entries: &[Entry]) -> impl Iterator<Item = Seg> {
    entries.iter().enumerate().map(|(i, t)| {
        let kind = match t {
            t if t.current => Kind::Current,
            t if t.bell => Kind::Bell,
            t if t.activity => Kind::Activity,
            t if t.alternate => Kind::Alternate,
            _ => Kind::Other,
        };
        seg(format!(" {} {} ", i + 1, t.name), kind)
    })
}

/// Drops empty pieces and the whitespace that empty fields leave at either end.
fn trim(segs: Vec<Seg>) -> Vec<Seg> {
    let mut segs: Vec<Seg> = segs.into_iter().filter(|s| !s.text.is_empty()).collect();
    while let Some(s) = segs.first_mut().filter(|s| !s.kind.is_term()) {
        s.text = s.text.trim_start().into();
        if !s.text.is_empty() {
            break;
        }
        segs.remove(0);
    }
    while let Some(s) = segs.last_mut().filter(|s| !s.kind.is_term()) {
        s.text = s.text.trim_end().into();
        if !s.text.is_empty() {
            break;
        }
        segs.pop();
    }
    segs
}

/// Fits `segs` in `width` columns by showing only the terminals around the current one, with `<` `>` for the rest.
pub fn fit(segs: Vec<Seg>, width: usize) -> Vec<Seg> {
    let w = |s: &Seg| s.text.width();
    if segs.iter().map(w).sum::<usize>() <= width {
        return segs;
    }
    let terms: Vec<usize> = (0..segs.len()).filter(|&i| segs[i].kind.is_term()).collect();
    if terms.is_empty() {
        return segs;
    }
    let fixed: usize = segs.iter().filter(|s| !s.kind.is_term()).map(w).sum();
    let budget = width.saturating_sub(fixed + 2);
    let at = terms.iter().position(|&i| segs[i].kind == Kind::Current).unwrap_or(0);
    let (mut lo, mut hi) = (at, at + 1);
    let mut used = w(&segs[terms[at]]);
    loop {
        let mut grew = false;
        if hi < terms.len() && used + w(&segs[terms[hi]]) <= budget {
            used += w(&segs[terms[hi]]);
            hi += 1;
            grew = true;
        }
        if lo > 0 && used + w(&segs[terms[lo - 1]]) <= budget {
            used += w(&segs[terms[lo - 1]]);
            lo -= 1;
            grew = true;
        }
        if !grew {
            break;
        }
    }
    let mut out = Vec::new();
    for (i, s) in segs.into_iter().enumerate() {
        let Some(j) = terms.iter().position(|&t| t == i) else {
            out.push(s);
            continue;
        };
        if j == lo && lo > 0 {
            out.push(seg("<", Kind::More));
        }
        if (lo..hi).contains(&j) {
            out.push(s);
        }
        if j + 1 == hi && hi < terms.len() {
            out.push(seg(">", Kind::More));
        }
    }
    out
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
            workflow: "dev",
            terms: vec![
                Entry { current: true, ..entry("web") },
                Entry { alternate: true, ..entry("api") },
                Entry { activity: true, bell: true, ..entry("claude") },
            ],
            ..Default::default()
        }
    }

    fn plain(segs: &[Seg]) -> String {
        segs.iter().map(|s| s.text.as_str()).collect()
    }

    fn t(s: &str, info: &Info) -> String {
        plain(&Template::parse(s).unwrap().expand(info))
    }

    #[test]
    fn fields() {
        assert_eq!(t("{mode}", &info()), "-- TERMINAL --");
        assert_eq!(t("{index}/{total} {name}", &info()), "1/3 web");
        assert_eq!(t("{terms}", &info()), " 1 web  2 api  3 claude ");
        assert_eq!(t("[{name}] {workflow}", &info()), "[web] dev");
        let kinds: Vec<Kind> = Template::parse("{terms}").unwrap().expand(&info()).iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![Kind::Current, Kind::Alternate, Kind::Bell]);
    }

    #[test]
    fn empty_fields_trim() {
        let list = Info { terms: vec![entry("web")], ..Default::default() };
        assert_eq!(t("{modified} {terms}", &list), " 1 web ");
        assert_eq!(t("{index} {name}", &list), "");
        let modified = Info { modified: true, ..list };
        assert_eq!(t("{modified} {terms}", &modified), "[+]  1 web ");
    }

    #[test]
    fn fit_slides_around_current() {
        let names = ["a", "b", "c", "d", "e", "f"];
        let mut info = Info { terms: names.iter().map(|n| entry(n)).collect(), ..Default::default() };
        let segs = |info: &Info| Template::parse("[+] {terms}").unwrap().expand(info);
        // each tab is 5 wide, the prefix 4
        assert_eq!(plain(&fit(segs(&info), 34)), "[+]  1 a  2 b  3 c  4 d  5 e  6 f ");
        assert_eq!(plain(&fit(segs(&info), 21)), "[+]  1 a  2 b  3 c >");
        info.terms[3].current = true;
        assert_eq!(plain(&fit(segs(&info), 21)), "[+] < 3 c  4 d  5 e >");
        info.terms[3].current = false;
        info.terms[5].current = true;
        assert_eq!(plain(&fit(segs(&info), 21)), "[+] < 4 d  5 e  6 f ");
        // the current tab shows even when nothing else fits
        assert_eq!(plain(&fit(segs(&info), 3)), "[+] < 6 f ");
    }

    #[test]
    fn bad_templates() {
        assert!(Template::parse("{nope}").is_err());
        assert!(Template::parse("{mode").is_err());
        assert!(Template::parse("").unwrap().0.is_empty());
    }
}
