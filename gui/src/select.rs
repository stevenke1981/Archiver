//! File masks for WinRAR's "select group" / "deselect group" (numpad + / −).

/// True when `name` matches `masks`: one or more masks separated by `;` or whitespace, each
/// with `*` (any run of characters) and `?` (one character), case-insensitive. Every mask must
/// match the whole name (like WinRAR: `*.txt`, `report?.doc`; a mask without wildcards is an
/// exact name). An empty / blank `masks` matches nothing.
pub fn matches(masks: &str, name: &str) -> bool {
    let name: Vec<char> = name.to_lowercase().chars().collect();
    masks
        .split(|c: char| c == ';' || c.is_whitespace())
        .filter(|m| !m.is_empty())
        .any(|m| {
            let m = m.to_lowercase();
            // WinRAR quirk: `*.*` selects every name, including names without a dot.
            m == "*.*" || glob(&m.chars().collect::<Vec<char>>(), &name)
        })
}

/// Whole-string glob match over characters: `*` any run (including empty), `?` one character.
/// Iterative with single-star backtracking, so no recursion and no exponential blow-up.
fn glob(pat: &[char], s: &[char]) -> bool {
    let (mut p, mut i) = (0, 0);
    // position of the last `*` in the pattern and the text index it currently absorbs up to
    let (mut star, mut mark) = (None, 0);
    while i < s.len() {
        if p < pat.len() && pat[p] == '*' {
            star = Some(p);
            mark = i;
            p += 1;
        } else if p < pat.len() && (pat[p] == '?' || pat[p] == s[i]) {
            p += 1;
            i += 1;
        } else if let Some(sp) = star {
            // let the last star absorb one more character and retry
            p = sp + 1;
            mark += 1;
            i = mark;
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn star_and_question_mark() {
        assert!(matches("*.txt", "notes.txt"));
        assert!(matches("*.txt", ".txt"));
        assert!(!matches("*.txt", "notes.txt.bak"));
        assert!(!matches("*.txt", "notes.doc"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "ac"));
        assert!(!matches("a?c", "abbc"));
        assert!(matches("report?.doc", "report1.doc"));
        assert!(matches("*", "anything at all"));
        assert!(matches("*", ""));
    }

    #[test]
    fn backtracking_across_several_stars() {
        assert!(matches("a*b*c", "axxbyyc"));
        assert!(matches("a*b*c", "abbbc"));
        assert!(!matches("a*b*c", "axxbyy"));
        assert!(matches("*a*a*a*", "banana"));
        assert!(!matches("*a*a*a*", "bnn"));
        assert!(matches("*ab", "aaaab"));
    }

    #[test]
    fn exact_name_without_wildcards() {
        assert!(matches("README", "readme"));
        assert!(!matches("README", "README.md"));
    }

    #[test]
    fn star_dot_star_matches_everything_including_no_dot() {
        assert!(matches("*.*", "README"));
        assert!(matches("*.*", "a.b.c"));
        assert!(matches("*.*", ".hidden"));
        assert!(matches("*.*", "報告"));
    }

    #[test]
    fn multiple_masks_separated_by_semicolon_or_whitespace() {
        assert!(matches("*.jpg;*.png", "photo.PNG"));
        assert!(matches("*.jpg;*.png", "photo.jpg"));
        assert!(!matches("*.jpg;*.png", "photo.gif"));
        assert!(matches("*.jpg *.png", "photo.png"));
        assert!(matches(" ;; *.zip ;", "a.zip"));
        assert!(!matches(" ;; *.zip ;", "a.7z"));
    }

    #[test]
    fn case_insensitive() {
        assert!(matches("*.TXT", "Notes.txt"));
        assert!(matches("NOTES.*", "notes.TXT"));
    }

    #[test]
    fn cjk_names_match_by_character() {
        assert!(matches("報告*.docx", "報告 2026.docx"));
        assert!(!matches("報告*.docx", "報表.docx"));
        assert!(matches("?告.txt", "報告.txt"));
        assert!(!matches("?.txt", "報告.txt"));
        assert!(matches("專案*", "專案 一"));
    }

    #[test]
    fn empty_or_blank_masks_match_nothing() {
        assert!(!matches("", "a.txt"));
        assert!(!matches("   ", "a.txt"));
        assert!(!matches(";;", "a.txt"));
        assert!(!matches("", ""));
    }
}
