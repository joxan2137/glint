//! Caret editing on a `String` with byte-index carets on grapheme-cluster boundaries (an emoji with modifiers, a
//! flag or a letter with combining marks moves and deletes as one), plus UTF-16 conversions for DirectWrite.

use unicode_segmentation::UnicodeSegmentation;

pub fn insert(text: &mut String, caret: &mut usize, s: &str) {
    *caret = snap(text, *caret);
    let clean: String = s.chars().filter(|c| *c == '\n' || !c.is_control()).collect();
    text.insert_str(*caret, &clean);
    *caret += clean.len();
}

pub fn backspace(text: &mut String, caret: &mut usize) {
    *caret = snap(text, *caret);
    let previous = prev_boundary(text, *caret);
    text.replace_range(previous..*caret, "");
    *caret = previous;
}

pub fn delete(text: &mut String, caret: usize) {
    let caret = snap(text, caret);
    let next = next_boundary(text, caret);
    text.replace_range(caret..next, "");
}

pub fn prev_boundary(text: &str, caret: usize) -> usize {
    let caret = snap(text, caret);
    text[..caret].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
}

pub fn next_boundary(text: &str, caret: usize) -> usize {
    let caret = snap(text, caret);
    text[caret..].graphemes(true).next().map_or(caret, |g| caret + g.len())
}

/// The grapheme boundary at or before `byte` (clamped to the text).
pub fn snap(text: &str, byte: usize) -> usize {
    if byte >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true).map(|(i, _)| i).take_while(|i| *i <= byte).last().unwrap_or(0)
}

pub fn line_start(text: &str, caret: usize) -> usize {
    let caret = snap(text, caret);
    text[..caret].rfind('\n').map_or(0, |i| i + 1)
}

pub fn line_end(text: &str, caret: usize) -> usize {
    let caret = snap(text, caret);
    text[caret..].find('\n').map_or(text.len(), |i| caret + i)
}

/// The caret one line up or down at the same column (in chars), or the text start/end past the first/last line.
pub fn vertical(text: &str, caret: usize, down: bool) -> usize {
    let caret = snap(text, caret);
    let start = line_start(text, caret);
    let column = text[start..caret].graphemes(true).count();
    let target_start = if down {
        let end = line_end(text, caret);
        if end == text.len() {
            return text.len();
        }
        end + 1
    } else {
        if start == 0 {
            return 0;
        }
        line_start(text, start - 1)
    };
    let target_end = line_end(text, target_start);
    text[target_start..target_end].grapheme_indices(true).nth(column).map_or(target_end, |(i, _)| target_start + i)
}

pub fn utf16_index(text: &str, byte: usize) -> u32 {
    text[..byte.min(text.len())].encode_utf16().count() as u32
}

/// The grapheme boundary at or before UTF-16 index `utf16`.
pub fn byte_index(text: &str, utf16: u32) -> usize {
    let mut units = 0u32;
    for (i, c) in text.char_indices() {
        if units >= utf16 {
            return snap(text, i);
        }
        units += c.len_utf16() as u32;
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_deleting() {
        let mut text = String::new();
        let mut caret = 0;
        insert(&mut text, &mut caret, "héllo");
        assert_eq!((text.as_str(), caret), ("héllo", 6));
        backspace(&mut text, &mut caret);
        backspace(&mut text, &mut caret);
        assert_eq!(text, "hél");
        caret = prev_boundary(&text, caret);
        caret = prev_boundary(&text, caret);
        assert_eq!(caret, 1);
        delete(&mut text, caret);
        assert_eq!(text, "hl");
        insert(&mut text, &mut caret, "\u{7}x\ny");
        assert_eq!(text, "hx\nyl", "control characters other than newline are dropped");
    }

    #[test]
    fn lines_and_columns() {
        let text = "first\nab\nthird line";
        let caret = 4;
        assert_eq!(line_start(text, caret), 0);
        assert_eq!(line_end(text, caret), 5);
        let down = vertical(text, caret, true);
        assert_eq!(down, 8, "column clamps to the shorter line");
        let down2 = vertical(text, down, true);
        assert_eq!(&text[line_start(text, down2)..down2], "th");
        assert_eq!(vertical(text, down2, true), text.len());
        assert_eq!(vertical(text, 2, false), 0);
        assert_eq!(vertical(text, down2, false), 8);
    }

    #[test]
    fn utf16_round_trip() {
        let text = "a😀b";
        assert_eq!(utf16_index(text, text.len()), 4);
        assert_eq!(utf16_index(text, 5), 3);
        assert_eq!(byte_index(text, 3), 5);
        assert_eq!(byte_index(text, 1), 1);
        assert_eq!(byte_index(text, 99), text.len());
    }

    #[test]
    fn graphemes_move_and_delete_as_one() {
        let family = "\u{1F469}\u{200D}\u{1F469}\u{200D}\u{1F467}";
        let flag = "\u{1F1F8}\u{1F1EA}";
        let mut text = format!("a{family}e\u{301}{flag}");
        let mut caret = text.len();
        backspace(&mut text, &mut caret);
        assert_eq!(text, format!("a{family}e\u{301}"), "a flag is one grapheme");
        backspace(&mut text, &mut caret);
        assert_eq!(text, format!("a{family}"), "a letter with a combining accent is one grapheme");
        assert_eq!(prev_boundary(&text, caret), 1);
        assert_eq!(next_boundary(&text, 1), text.len());
        delete(&mut text, 1);
        assert_eq!(text, "a");
        assert_eq!(snap("e\u{301}x", 1), 0, "inside a cluster snaps back to its start");
        assert_eq!(byte_index("e\u{301}x", 1), 0);
    }

    #[test]
    fn boundaries_at_the_ends() {
        let mut text = String::from("ab");
        let mut caret = 0;
        backspace(&mut text, &mut caret);
        assert_eq!((text.as_str(), caret), ("ab", 0));
        delete(&mut text, 2);
        assert_eq!(text, "ab");
        assert_eq!(next_boundary("ab", 2), 2);
    }
}
