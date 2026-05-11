//! Shared helpers for chunking long outbound text into channel-sized pieces.
//!
//! Telegram caps `sendMessage` at 4096 characters and Feishu's text message
//! body has a similar practical ceiling. We split at paragraph / line / word
//! boundaries when one is available within the cap, falling back to a hard
//! character break for continuous strings (URLs, base64, etc).

/// Split `text` into chunks that each fit within `max_chars`. Returns a single
/// element when the input already fits.
pub(super) fn split_text(text: &str, max_chars: usize) -> Vec<String> {
    if text.chars().count() <= max_chars {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.chars().count() <= max_chars {
            chunks.push(remaining.to_owned());
            break;
        }
        let split_at = pick_split(remaining, max_chars);
        let (head, tail) = remaining.split_at(split_at);
        chunks.push(head.trim_end().to_owned());
        remaining = tail.trim_start_matches(['\n', ' ']);
    }
    chunks
}

fn pick_split(text: &str, max_chars: usize) -> usize {
    let mut last_paragraph = None;
    let mut last_newline = None;
    let mut last_space = None;
    let mut prev_char = '\0';
    let mut byte_idx = 0;
    for (char_count, (idx, ch)) in text.char_indices().enumerate() {
        if char_count >= max_chars {
            break;
        }
        if prev_char == '\n' && ch == '\n' {
            last_paragraph = Some(idx + ch.len_utf8());
        } else if ch == '\n' {
            last_newline = Some(idx + ch.len_utf8());
        } else if ch == ' ' {
            last_space = Some(idx);
        }
        prev_char = ch;
        byte_idx = idx + ch.len_utf8();
    }
    last_paragraph
        .or(last_newline)
        .or(last_space)
        .unwrap_or(byte_idx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_short_text_through_unchanged() {
        assert_eq!(
            split_text("hello world", 4096),
            vec!["hello world".to_owned()]
        );
    }

    #[test]
    fn chunks_at_paragraph_boundary() {
        let para = "a".repeat(3000);
        let text = format!("{para}\n\n{para}");
        let chunks = split_text(&text, 4096);
        assert_eq!(chunks.len(), 2);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 4096);
        }
    }

    #[test]
    fn chunks_at_newline_when_no_paragraph_break() {
        let line = "b".repeat(2500);
        let text = format!("{line}\n{line}");
        let chunks = split_text(&text, 4096);
        assert_eq!(chunks.len(), 2);
    }

    #[test]
    fn hard_breaks_continuous_text() {
        let text = "x".repeat(4096 * 2 + 10);
        let chunks = split_text(&text, 4096);
        assert!(chunks.len() >= 3);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 4096);
        }
        let joined: String = chunks.join("");
        assert_eq!(joined.len(), text.len());
    }

    #[test]
    fn respects_custom_cap() {
        let text = "a".repeat(500);
        let chunks = split_text(&text, 200);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= 200);
        }
    }
}
