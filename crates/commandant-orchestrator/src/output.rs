//! The output a task keeps: its last bytes, each tagged with its stream.

use std::collections::VecDeque;

use commandant_proto::TaskOutput;

/// How much of a task's output is kept: its last 1 MiB.
pub const OUTPUT_CAP: usize = 1 << 20;

/// The last bytes of a task's output, in chunks of one stream each.
pub struct Tail {
    chunks: VecDeque<TaskOutput>,
    len: usize,
    cap: usize,
}

impl Default for Tail {
    fn default() -> Self {
        Self::with_cap(OUTPUT_CAP)
    }
}

impl Tail {
    fn with_cap(cap: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            len: 0,
            cap,
        }
    }

    /// Adds a chunk, joined to the last one if it is of the same stream, and
    /// drops the oldest bytes past the cap.
    pub fn push(&mut self, chunk: &TaskOutput) {
        if chunk.data.is_empty() {
            return;
        }
        match self.chunks.back_mut() {
            Some(last) if last.stream == chunk.stream => last.data.extend_from_slice(&chunk.data),
            _ => self.chunks.push_back(chunk.clone()),
        }
        self.len += chunk.data.len();
        while self.len > self.cap {
            let excess = self.len - self.cap;
            let first = self.chunks.front_mut().expect("len counts its bytes");
            if first.data.len() <= excess {
                self.len -= first.data.len();
                self.chunks.pop_front();
            } else {
                first.data.drain(..excess);
                self.len -= excess;
            }
        }
    }

    pub fn chunks(&self) -> Vec<TaskOutput> {
        self.chunks.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use commandant_proto::OutputStream;

    use super::*;
    use OutputStream::{Reasoning, Stderr, Stdout};

    fn chunk(stream: OutputStream, data: &str) -> TaskOutput {
        TaskOutput {
            task_id: "t".into(),
            stream: stream.into(),
            data: data.into(),
        }
    }

    fn pushed(cap: usize, chunks: &[(OutputStream, &str)]) -> Vec<(OutputStream, String)> {
        let mut tail = Tail::with_cap(cap);
        for (stream, data) in chunks {
            tail.push(&chunk(*stream, data));
        }
        tail.chunks()
            .into_iter()
            .map(|c| (c.stream(), String::from_utf8(c.data).unwrap()))
            .collect()
    }

    #[test]
    fn keeps_each_chunks_stream_joining_runs_of_one() {
        let kept = pushed(
            100,
            &[
                (Stdout, "a"),
                (Stdout, "b"),
                (Stderr, "c"),
                (Stdout, ""),
                (Reasoning, "d"),
                (Stdout, "e"),
            ],
        );
        let expected = [
            (Stdout, "ab"),
            (Stderr, "c"),
            (Reasoning, "d"),
            (Stdout, "e"),
        ];
        assert_eq!(kept, expected.map(|(s, d)| (s, d.to_string())));
    }

    #[test]
    fn keeps_only_the_last_bytes() {
        let kept = pushed(4, &[(Stdout, "abc"), (Stderr, "de"), (Stdout, "fgh")]);
        let expected = [(Stderr, "e"), (Stdout, "fgh")];
        assert_eq!(kept, expected.map(|(s, d)| (s, d.to_string())));

        let kept = pushed(4, &[(Stdout, "abcdefg")]);
        assert_eq!(kept, [(Stdout, "defg".to_string())]);
    }
}
