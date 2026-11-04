//! What the scans share about JavaScript and HTML text: where a name ends, which line a byte is
//! on, and the blanks (white space and comments) that may come between a name and its `(`.

/// Whether `c` can be part of a JavaScript name.
pub(crate) fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Whether the text before `at` ends in a name character, so that a name found at `at` is the
/// tail of a longer one (`eval` in `retrieval`).
pub(crate) fn joined(text: &str, at: usize) -> bool {
    text[..at].chars().next_back().is_some_and(is_ident)
}

/// The 1-based line of byte `at` of `text`.
pub(crate) fn line_of(text: &str, at: usize) -> usize {
    text[..at].matches('\n').count() + 1
}

/// `rest` after the white space and comments (`/* ... */`, `// ...` to the end of the line)
/// that open it. An unterminated block comment runs to the end of the text.
pub(crate) fn skip_blank(mut rest: &str) -> &str {
    loop {
        rest = rest.trim_start();
        if let Some(comment) = rest.strip_prefix("/*") {
            rest = comment.find("*/").map_or("", |end| &comment[end + 2..]);
        } else if let Some(comment) = rest.strip_prefix("//") {
            rest = comment.find('\n').map_or("", |end| &comment[end..]);
        } else {
            return rest;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanks_are_white_space_and_both_kinds_of_comment() {
        assert_eq!(skip_blank("  \n\t(x"), "(x");
        assert_eq!(skip_blank(" /* a */ /*b*/(x"), "(x");
        assert_eq!(skip_blank(" // a\n  (x"), "(x");
        assert_eq!(skip_blank(" /* never closed (x"), "");
        assert_eq!(skip_blank(" // to the end (x"), "");
        assert_eq!(skip_blank("x"), "x");
    }

    #[test]
    fn a_name_joined_to_the_text_before_it_is_part_of_a_longer_name() {
        for (text, at) in [("re_eval", 3), ("$eval", 1), ("xeval", 1), ("9eval", 1)] {
            assert!(joined(text, at), "{text}");
        }
        for (text, at) in [("eval", 0), (" eval", 1), (".eval", 1), ("(eval", 1)] {
            assert!(!joined(text, at), "{text}");
        }
        assert_eq!(line_of("a\nb\nc", 4), 3);
    }
}
