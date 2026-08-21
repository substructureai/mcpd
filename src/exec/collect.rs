use std::collections::VecDeque;

pub struct Collector {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    head_limit: usize,
    tail_limit: usize,
    total: usize,
}

impl Collector {
    /// Split evenly, matching Codex's `split_budget`.
    pub fn new(limit: usize) -> Self {
        let head_limit = limit / 2;
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            head_limit,
            tail_limit: limit - head_limit,
            total: 0,
        }
    }

    pub fn push(&mut self, mut chunk: &[u8]) {
        self.total += chunk.len();

        if self.head.len() < self.head_limit {
            let take = (self.head_limit - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
        }

        if chunk.is_empty() || self.tail_limit == 0 {
            return;
        }

        if chunk.len() >= self.tail_limit {
            self.tail.clear();
            self.tail.extend(&chunk[chunk.len() - self.tail_limit..]);
            return;
        }

        while self.tail.len() + chunk.len() > self.tail_limit {
            self.tail.pop_front();
        }
        self.tail.extend(chunk);
    }

    /// Borrows rather than consumes, so output collected so far survives a
    /// drain that had to be abandoned.
    pub fn render(&self) -> (String, bool) {
        let kept = self.head.len() + self.tail.len();
        let elided = self.total - kept;
        let tail: Vec<u8> = self.tail.iter().copied().collect();

        if elided == 0 {
            let mut bytes = self.head.clone();
            bytes.extend(tail);
            return (String::from_utf8_lossy(&bytes).into_owned(), false);
        }

        let head = String::from_utf8_lossy(&self.head);
        let tail = String::from_utf8_lossy(&tail);
        (format!("{head}\n… {elided} bytes elided …\n{tail}"), true)
    }

    pub fn finish(self) -> (String, bool) {
        self.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]], limit: usize) -> (String, bool) {
        let mut c = Collector::new(limit);
        for chunk in chunks {
            c.push(chunk);
        }
        c.finish()
    }

    #[test]
    fn output_under_the_limit_is_untouched() {
        let (text, truncated) = collect(&[b"hello"], 100);
        assert_eq!(text, "hello");
        assert!(!truncated);
    }

    #[test]
    fn output_exactly_at_the_limit_is_untouched() {
        let (text, truncated) = collect(&[b"abcdef"], 6);
        assert_eq!(text, "abcdef");
        assert!(!truncated);
    }

    #[test]
    fn a_long_run_keeps_the_head_and_the_tail() {
        let mut body = b"HEADHEAD".to_vec();
        body.extend(std::iter::repeat_n(b'.', 980));
        body.extend(b"TAILTAILTAILTAIL".to_vec());
        let (text, truncated) = collect(&[&body], 24);
        assert!(truncated);
        assert!(text.starts_with("HEADHEAD"));
        assert!(text.ends_with("TAILTAILTAIL"));
        assert!(text.contains("980 bytes elided"));
    }

    #[test]
    fn the_budget_is_split_evenly_between_head_and_tail() {
        let body: Vec<u8> = std::iter::repeat_n(b'x', 1000).collect();
        let mut c = Collector::new(30);
        c.push(&body);
        let (text, _) = c.render();
        let kept: usize = text.matches('x').count();
        assert_eq!(kept, 30);
        assert_eq!(c.head.len(), 15);
        assert_eq!(c.tail.len(), 15);
    }

    #[test]
    fn an_odd_budget_gives_the_extra_byte_to_the_tail() {
        let mut c = Collector::new(9);
        c.push(&[b'x'; 100]);
        assert_eq!(c.head.len(), 4);
        assert_eq!(c.tail.len(), 5);
    }

    #[test]
    fn a_partial_render_does_not_consume_the_collector() {
        let mut c = Collector::new(100);
        c.push(b"first");
        assert_eq!(c.render().0, "first");
        c.push(b" second");
        assert_eq!(c.render().0, "first second");
    }

    #[test]
    fn the_boundary_does_not_depend_on_how_it_arrived() {
        let whole = collect(&[b"0123456789abcdefghij"], 10);
        let split = collect(&[b"01234", b"56789ab", b"cdefghij"], 10);
        assert_eq!(whole, split);
    }

    #[test]
    fn a_single_chunk_larger_than_the_limit_still_keeps_both_ends() {
        let (text, truncated) = collect(&[b"0123456789ABCDEFGHIJ"], 9);
        assert!(truncated);
        assert!(text.starts_with("0123"));
        assert!(text.ends_with("FGHIJ"));
    }

    #[test]
    fn the_elided_count_is_the_bytes_actually_dropped() {
        let (text, _) = collect(&[&[b'x'; 100]], 10);
        assert!(text.contains("90 bytes elided"));
    }

    #[test]
    fn a_split_character_does_not_produce_invalid_text() {
        let mut c = Collector::new(1000);
        c.push(&"é".as_bytes()[..1]);
        let (text, _) = c.finish();
        assert_eq!(text, "\u{fffd}");
    }

    #[test]
    fn binary_output_survives_as_replacement_characters() {
        let (text, truncated) = collect(&[&[0xff, 0xfe, 0x00]], 100);
        assert!(!truncated);
        assert_eq!(text.chars().count(), 3);
    }
}
