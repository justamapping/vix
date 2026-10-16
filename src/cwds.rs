/// The list's virtual text: each terminal's directory, as few trailing components as tell it apart from the
/// others' (like editor tabs with the same file name), hidden when it's just the terminal's name.
/// `terms` is `(name, cwd)` with cwds already `~`-abbreviated.
pub fn labels(terms: &[(&str, &str)]) -> Vec<Option<String>> {
    let parts: Vec<Vec<&str>> = terms.iter().map(|(_, cwd)| components(cwd)).collect();
    let mut lens = vec![1; terms.len()];
    let label = |i: usize, len: usize| parts[i][parts[i].len().saturating_sub(len)..].join("/");
    loop {
        let mut grow = vec![false; terms.len()];
        for i in 0..terms.len() {
            for j in i + 1..terms.len() {
                if terms[i].1 != terms[j].1 && label(i, lens[i]) == label(j, lens[j]) {
                    (grow[i], grow[j]) = (true, true);
                }
            }
        }
        let mut grew = false;
        for (i, g) in grow.into_iter().enumerate() {
            if g && lens[i] < parts[i].len() {
                lens[i] += 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    (0..terms.len())
        .map(|i| {
            let text = label(i, lens[i]);
            let text = if text.is_empty() { "/".into() } else { text };
            (text != terms[i].0).then_some(text)
        })
        .collect()
}

/// `/usr/bin` keeps its root as an empty first component, so the full path joins back with its leading `/`.
fn components(cwd: &str) -> Vec<&str> {
    match cwd {
        "/" => vec![""],
        _ => cwd.trim_end_matches('/').split('/').collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(terms: &[(&str, &str)], want: &[Option<&str>]) {
        assert_eq!(labels(terms).iter().map(Option::as_deref).collect::<Vec<_>>(), want);
    }

    #[test]
    fn last_component_when_unique() {
        check(&[("web", "~/programs/site"), ("db", "/var/lib/pg")], &[Some("site"), Some("pg")]);
    }

    #[test]
    fn grows_until_distinct() {
        let terms = [("a", "~/vix/app"), ("b", "~/datatypes/app"), ("c", "~/vix/app"), ("d", "~/notes")];
        check(&terms, &[Some("vix/app"), Some("datatypes/app"), Some("vix/app"), Some("notes")]);
    }

    #[test]
    fn hidden_when_it_is_the_name() {
        check(&[("vix", "~/programs/vix"), ("logs", "~/programs/vix")], &[None, Some("vix")]);
    }

    #[test]
    fn home_and_root() {
        check(&[("a", "~"), ("b", "/"), ("c", "/usr/bin"), ("d", "~/bin")], &[Some("~"), Some("/"), Some("usr/bin"), Some("~/bin")]);
    }
}
