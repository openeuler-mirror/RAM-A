//! 本文件验证 model/corpus/loader 模块的行为。

use std::io::Cursor;

use super::*;

#[test]
fn loads_valid_records_and_explicitly_skips_every_invalid_line() {
    let input = concat!(
            "{\"captured_at\":\"now\",\"comparison\":{\"endpoint_key\":\"chat\"},\"payload\":{\"model\":\"m\"}}\n",
            "\n",
            "not-json\n",
            "[]\n",
            "{\"source\":\"x\"}\n",
            "{\"payload\":42}\n"
        );
    let result = CorpusLoader::default()
        .load(Cursor::new(input))
        .expect("load succeeds");
    assert_eq!(result.corpus.records.len(), 1);
    assert_eq!(result.skipped.len(), 5);
    assert_eq!(result.corpus.records[0].captured_at.as_deref(), Some("now"));
}

#[test]
fn parses_response_usage_with_aliases_and_latency() {
    let input = concat!(
        "{\"captured_at\":\"now\",\"comparison\":{\"endpoint_key\":\"chat\"},",
        "\"payload\":{\"model\":\"m\",\"_actrail_response_usage\":{",
        "\"input_tokens\":1200,\"cached_tokens\":900,\"output_tokens\":50,",
        "\"ttft_ms\":320,\"total_ms\":1850}}}\n"
    );
    let result = CorpusLoader::default()
        .load(Cursor::new(input))
        .expect("load succeeds");
    let usage = result.corpus.records[0]
        .response_usage
        .as_ref()
        .expect("usage parsed from aliases");
    assert_eq!(usage.prompt_tokens, 1200);
    assert_eq!(usage.cached_tokens, 900);
    assert_eq!(usage.completion_tokens, 50);
    assert_eq!(usage.ttft_ms, Some(320));
    assert_eq!(usage.total_ms, Some(1850));
}

#[test]
fn usage_without_total_input_is_treated_as_absent() {
    let input = concat!(
        "{\"captured_at\":\"now\",\"comparison\":{\"endpoint_key\":\"chat\"},",
        "\"payload\":{\"model\":\"m\",\"_actrail_response_usage\":{",
        "\"cached_tokens\":900,\"ttft_ms\":320}}}\n"
    );
    let result = CorpusLoader::default()
        .load(Cursor::new(input))
        .expect("load succeeds");
    assert!(
        result.corpus.records[0].response_usage.is_none(),
        "缺少总输入数时按没有 usage 处理，避免产出看起来真实的 0%"
    );
}

#[test]
fn omitted_latency_stays_absent_instead_of_zero() {
    let input = concat!(
        "{\"captured_at\":\"now\",\"comparison\":{\"endpoint_key\":\"chat\"},",
        "\"payload\":{\"model\":\"m\",\"_actrail_response_usage\":{\"prompt_tokens\":100}}}\n"
    );
    let result = CorpusLoader::default()
        .load(Cursor::new(input))
        .expect("load succeeds");
    let usage = result.corpus.records[0]
        .response_usage
        .as_ref()
        .expect("usage");
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.ttft_ms, None);
    assert_eq!(usage.total_ms, None);
}

#[test]
fn stable_id_ignores_outer_object_key_order() {
    let a = "{\"comparison\":{\"endpoint_key\":\"chat\"},\"payload\":{\"model\":\"m\",\"messages\":[]}}";
    let b = "{\"payload\":{\"messages\":[],\"model\":\"m\"},\"comparison\":{\"endpoint_key\":\"chat\"}}";
    let a = CorpusLoader::default()
        .load(Cursor::new(a))
        .expect("load succeeds");
    let b = CorpusLoader::default()
        .load(Cursor::new(b))
        .expect("load succeeds");
    assert_eq!(a.corpus.records[0].id, b.corpus.records[0].id);
}

#[test]
fn duplicate_records_keep_frequency_with_permutation_stable_ids() {
    let a = "{\"source\":\"a\",\"comparison\":{\"endpoint_key\":\"chat\"},\"payload\":{\"model\":\"m\"}}";
    let b = "{\"source\":\"b\",\"comparison\":{\"endpoint_key\":\"chat\"},\"payload\":{\"model\":\"m\"}}";
    let first = CorpusLoader::default()
        .load(Cursor::new(format!("{a}\n{b}\n{a}")))
        .expect("load succeeds");
    let second = CorpusLoader::default()
        .load(Cursor::new(format!("{a}\n{a}\n{b}")))
        .expect("load succeeds");
    let mut first_ids: Vec<_> = first
        .corpus
        .records
        .iter()
        .map(|record| record.id.clone())
        .collect();
    let mut second_ids: Vec<_> = second
        .corpus
        .records
        .iter()
        .map(|record| record.id.clone())
        .collect();
    first_ids.sort();
    second_ids.sort();
    assert_eq!(first_ids, second_ids);
    assert_eq!(first_ids.len(), 3);
    assert_ne!(first_ids[0], first_ids[1]);
}

#[test]
fn rejects_empty_or_non_string_comparison_keys() {
    let input = concat!(
        "{\"comparison\":{\"endpoint_key\":\"   \"},\"payload\":{}}\n",
        "{\"comparison\":{\"endpoint_key\":\"chat\",\"agent_key\":42},\"payload\":{}}\n",
        "{\"comparison\":{\"endpoint_key\":\"chat\",\"kv_namespace\":\" \"},\"payload\":{}}"
    );
    let result = CorpusLoader::default()
        .load(Cursor::new(input))
        .expect("load succeeds");
    assert!(result.corpus.records.is_empty());
    assert!(matches!(
        result.skipped[0].reason,
        CorpusSkipReason::EmptyEndpointKey
    ));
    assert!(matches!(
        result.skipped[1].reason,
        CorpusSkipReason::InvalidComparisonKey { .. }
    ));
    assert!(matches!(
        result.skipped[2].reason,
        CorpusSkipReason::EmptyComparisonKey { .. }
    ));
}

#[test]
fn enforces_record_size_and_count_boundaries() {
    let at_limit = "{\"comparison\":{\"endpoint_key\":\"x\"},\"payload\":{}}";
    let limits = CorpusConfig {
        max_record_bytes: at_limit.len(),
        max_records: 1,
    };
    let input = format!("{at_limit}\n{at_limit} ");
    let result = CorpusLoader::new(limits)
        .load(Cursor::new(input))
        .expect("load succeeds");
    assert_eq!(result.corpus.records.len(), 1);
    assert!(matches!(
        result.skipped[0].reason,
        CorpusSkipReason::RecordTooLarge { .. }
    ));
}

#[test]
fn read_failures_abort_loading_instead_of_retrying_forever() {
    // Emulates a persistent read error (e.g. Windows ERROR_LOCK_VIOLATION
    // on the receiver's live output): `split` yields the error again on
    // every subsequent `next()` call, so skipping the line would loop
    // forever at the same offset.
    struct FailingReader<'a> {
        remaining: &'a [u8],
    }
    impl io::Read for FailingReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let copied = self.remaining.len().min(buf.len());
            buf[..copied].copy_from_slice(&self.remaining[..copied]);
            self.remaining = &self.remaining[copied..];
            Ok(copied)
        }
    }
    impl BufRead for FailingReader<'_> {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            if self.remaining.is_empty() {
                return Err(io::Error::other("injected read failure"));
            }
            Ok(self.remaining)
        }
        fn consume(&mut self, n: usize) {
            self.remaining = &self.remaining[n..];
        }
    }

    let input = "{\"comparison\":{\"endpoint_key\":\"chat\"},\"payload\":{\"model\":\"m\"}}\n";
    let error = CorpusLoader::default()
        .load(FailingReader {
            remaining: input.as_bytes(),
        })
        .expect_err("persistent read failures must abort");
    assert_eq!(error.input_line, 2);
    assert!(error.source.to_string().contains("injected read failure"));
}
