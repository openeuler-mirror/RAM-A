use anyhow::Result;
use tiktoken_rs::{cl100k_base, CoreBPE};

pub const CHUNK_TOKENIZER_NAME: &str = "cl100k_base";

pub trait TokenCounter: Send + Sync {
    fn count_tokens(&self, text: &str) -> Result<usize>;
    fn name(&self) -> &str;
}

pub struct Cl100kTokenCounter {
    bpe: CoreBPE,
}

impl Cl100kTokenCounter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            bpe: cl100k_base()?,
        })
    }
}

impl TokenCounter for Cl100kTokenCounter {
    fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(self.bpe.encode_ordinary(text).len())
    }

    fn name(&self) -> &str {
        CHUNK_TOKENIZER_NAME
    }
}

#[cfg(test)]
pub struct TestTokenCounter;

#[cfg(test)]
impl TokenCounter for TestTokenCounter {
    fn count_tokens(&self, text: &str) -> Result<usize> {
        Ok(test_token_count(text))
    }

    fn name(&self) -> &str {
        "test-token-counter"
    }
}

#[cfg(test)]
fn test_token_count(content: &str) -> usize {
    let mut tokens = 0usize;
    let mut ascii_run = 0usize;

    for ch in content.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            ascii_run += 1;
            continue;
        }

        tokens += test_ascii_run_tokens(ascii_run);
        ascii_run = 0;

        if ch.is_whitespace() {
            continue;
        }
        tokens += 1;
    }

    tokens + test_ascii_run_tokens(ascii_run)
}

#[cfg(test)]
fn test_ascii_run_tokens(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        len.div_ceil(4)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cl100k_counter_matches_reference_tiktoken_counts() {
        let counter = Cl100kTokenCounter::new().expect("cl100k counter");

        assert_eq!(counter.name(), CHUNK_TOKENIZER_NAME);
        assert_eq!(counter.count_tokens("hello").unwrap(), 1);
        assert_eq!(counter.count_tokens("hello world").unwrap(), 2);
    }
}
