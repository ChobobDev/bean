//! Splitting long replies into Discord-sized messages.

/// Characters kept free on each page for the fence that closes a code
/// block cut off by the split.
const FENCE_CLOSE: &str = "\n```";

/// Longest opening fence (e.g. "```rust") that gets carried over to the
/// next page; anything longer is replaced with a plain fence.
const MAX_FENCE_LEN: usize = 32;

/// Splits `text` into pages of at most `max_chars` characters.
///
/// Splits prefer line breaks, then spaces, so words aren't cut in half. A
/// code block that spans a split is closed at the end of one page and
/// reopened, with the same language tag, at the start of the next.
///
/// Splitting is stable as text grows: once a page is followed by another,
/// its content never changes. That lets a streaming reply edit only its
/// last message.
pub fn split_pages(text: &str, max_chars: usize) -> Vec<String> {
    let mut pages = Vec::new();
    let mut rest = text;
    let mut carried_fence: Option<String> = None;

    while !rest.trim().is_empty() {
        let prefix = carried_fence.take().map(|f| f + "\n").unwrap_or_default();
        let budget = max_chars.saturating_sub(prefix.chars().count() + FENCE_CLOSE.len());

        if rest.chars().count() <= budget {
            pages.push(prefix + rest);
            break;
        }

        let cut = find_cut(rest, budget.max(1));
        let (head, tail) = rest.split_at(cut);
        let mut page = prefix + head.trim_end();
        if let Some(fence) = open_fence(&page) {
            page.push_str(FENCE_CLOSE);
            carried_fence = Some(fence);
        }
        pages.push(page);
        rest = tail.trim_start_matches([' ', '\n']);
    }
    pages
}

/// Byte index at which to split `text` so the head has at most `budget`
/// characters.
fn find_cut(text: &str, budget: usize) -> usize {
    let hard = text
        .char_indices()
        .nth(budget)
        .map_or(text.len(), |(i, _)| i);
    let window = &text[..hard];
    // Only accept a soft break in the second half of the window, so a stray
    // early newline doesn't produce a tiny page.
    let soft = |i: usize| (i > hard / 2).then_some(i);
    window
        .rfind('\n')
        .and_then(soft)
        .or_else(|| window.rfind(' ').and_then(soft))
        .unwrap_or(hard)
}

/// Returns the opening fence line if `text` ends inside a code block.
fn open_fence(text: &str) -> Option<String> {
    let mut open: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim_start();
        if line.starts_with("```") {
            open = match open {
                Some(_) => None,
                None => Some(line),
            };
        }
    }
    open.map(|line| {
        if line.chars().count() <= MAX_FENCE_LEN {
            line.to_string()
        } else {
            "```".to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_fits(pages: &[String], max: usize) {
        for page in pages {
            assert!(page.chars().count() <= max, "page too long: {}", page.len());
            assert!(!page.trim().is_empty(), "empty page");
        }
    }

    #[test]
    fn short_text_is_one_page() {
        assert_eq!(split_pages("hello", 100), vec!["hello"]);
    }

    #[test]
    fn empty_text_has_no_pages() {
        assert!(split_pages("", 100).is_empty());
        assert!(split_pages(" \n ", 100).is_empty());
    }

    #[test]
    fn splits_on_line_breaks() {
        let text = "first line here\nsecond line here\nthird line here";
        let pages = split_pages(text, 40);
        assert_eq!(
            pages,
            vec!["first line here\nsecond line here", "third line here"]
        );
    }

    #[test]
    fn splits_on_spaces_when_no_line_breaks() {
        let text = "alpha beta gamma delta epsilon zeta eta theta";
        let pages = split_pages(text, 24);
        assert_fits(&pages, 24);
        assert_eq!(pages.join(" "), text);
    }

    #[test]
    fn hard_splits_unbroken_text() {
        let text = "x".repeat(50);
        let pages = split_pages(&text, 20);
        assert_fits(&pages, 20);
        assert_eq!(pages.concat(), text);
    }

    #[test]
    fn never_splits_inside_a_multibyte_character() {
        let text = "가".repeat(30);
        let pages = split_pages(&text, 10);
        assert_fits(&pages, 10);
        assert_eq!(pages.concat(), text);
    }

    #[test]
    fn reopens_code_blocks_across_pages() {
        let code: String = (0..20).map(|i| format!("let x{i} = {i};\n")).collect();
        let text = format!("Here you go:\n```rust\n{code}```\nDone.");
        let pages = split_pages(&text, 120);
        assert_fits(&pages, 120);
        assert!(pages.len() > 1);
        for page in &pages {
            let fences = page
                .lines()
                .filter(|l| l.trim_start().starts_with("```"))
                .count();
            assert_eq!(fences % 2, 0, "unbalanced fences in {page:?}");
        }
        assert!(pages[1].starts_with("```rust\n"));
    }

    #[test]
    fn earlier_pages_are_stable_as_text_grows() {
        let full: String = (0..200).map(|i| format!("word{i} ")).collect();
        let mut seen: Vec<String> = Vec::new();
        for end in (0..full.len()).step_by(7) {
            let pages = split_pages(&full[..end], 100);
            // Every page except the last is final and must not change.
            for (i, page) in pages.iter().enumerate().take(pages.len().saturating_sub(1)) {
                match seen.get(i) {
                    Some(prev) => assert_eq!(prev, page, "page {i} changed"),
                    None => seen.push(page.clone()),
                }
            }
        }
    }
}
