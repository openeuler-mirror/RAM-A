//! 本文件定义报告语料筛选和有界的最近记录选择。

use std::{
    cmp::{Ordering, Reverse},
    collections::{BTreeSet, BinaryHeap},
    num::NonZeroUsize,
};

use anyhow::Result;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[derive(Clone, Debug)]
pub struct CaptureTimeRange {
    from: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
}

impl CaptureTimeRange {
    pub fn new(from: Option<OffsetDateTime>, until: Option<OffsetDateTime>) -> Result<Self> {
        anyhow::ensure!(
            !matches!((from, until), (Some(from), Some(until)) if from >= until),
            "capture range start must precede its end"
        );
        Ok(Self { from, until })
    }

    pub fn from(&self) -> Option<OffsetDateTime> {
        self.from
    }

    pub fn until(&self) -> Option<OffsetDateTime> {
        self.until
    }

    fn contains(&self, timestamp: OffsetDateTime) -> bool {
        self.from.is_none_or(|from| timestamp >= from)
            && self.until.is_none_or(|until| timestamp < until)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ReportSelection {
    sessions: BTreeSet<String>,
    time_ranges: Vec<CaptureTimeRange>,
    last_n: Option<NonZeroUsize>,
}

impl ReportSelection {
    pub fn new(
        sessions: Vec<String>,
        time_ranges: Vec<CaptureTimeRange>,
        last_n: Option<NonZeroUsize>,
    ) -> Self {
        Self {
            sessions: sessions.into_iter().collect(),
            time_ranges,
            last_n,
        }
    }

    pub(crate) fn is_all(&self) -> bool {
        self.sessions.is_empty() && self.time_ranges.is_empty() && self.last_n.is_none()
    }

    pub(super) fn matches(
        &self,
        session: Option<&str>,
        captured_at: Option<&str>,
    ) -> Option<CaptureMatch> {
        if !self.sessions.is_empty()
            && !session.is_some_and(|session| self.sessions.contains(session))
        {
            return None;
        }
        if self.time_ranges.is_empty() && self.last_n.is_none() {
            return Some(CaptureMatch::Include);
        }
        let captured_at = OffsetDateTime::parse(captured_at?, &Rfc3339).ok()?;
        if !self.time_ranges.is_empty()
            && !self
                .time_ranges
                .iter()
                .any(|range| range.contains(captured_at))
        {
            return None;
        }
        Some(match self.last_n {
            Some(count) => CaptureMatch::Latest {
                captured_at,
                count: count.get(),
            },
            None => CaptureMatch::Include,
        })
    }
}

pub(super) enum CaptureMatch {
    Include,
    Latest {
        captured_at: OffsetDateTime,
        count: usize,
    },
}

pub(super) struct SelectionCollector<T> {
    max_records: usize,
    accepted: usize,
    latest: BinaryHeap<Reverse<RankedCapture<T>>>,
}

impl<T> SelectionCollector<T> {
    pub(super) fn new(max_records: usize) -> Self {
        Self {
            max_records,
            accepted: 0,
            latest: BinaryHeap::new(),
        }
    }

    pub(super) fn accept(
        &mut self,
        matched: CaptureMatch,
        position: usize,
        record: T,
    ) -> Result<Option<T>> {
        let requested = match matched {
            CaptureMatch::Include => usize::MAX,
            CaptureMatch::Latest { count, .. } => count,
        };
        if requested > self.max_records {
            anyhow::ensure!(
                self.accepted < self.max_records,
                "capture selection exceeds max_records ({})",
                self.max_records
            );
            self.accepted += 1;
        }
        let CaptureMatch::Latest { captured_at, count } = matched else {
            return Ok(Some(record));
        };
        let candidate = Reverse(RankedCapture {
            captured_at,
            position,
            record,
        });
        if self.latest.len() < count {
            self.latest.push(candidate);
        } else if let Some(mut oldest) = self.latest.peek_mut() {
            if candidate < *oldest {
                *oldest = candidate;
            }
        }
        Ok(None)
    }

    pub(super) fn into_latest(self) -> Vec<(usize, T)> {
        let mut selected: Vec<_> = self
            .latest
            .into_iter()
            .map(|Reverse(record)| (record.position, record.record))
            .collect();
        selected.sort_unstable_by_key(|(position, _)| *position);
        selected
    }
}

struct RankedCapture<T> {
    captured_at: OffsetDateTime,
    position: usize,
    record: T,
}

impl<T> Ord for RankedCapture<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.captured_at, self.position).cmp(&(other.captured_at, other.position))
    }
}

impl<T> PartialOrd for RankedCapture<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> PartialEq for RankedCapture<T> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl<T> Eq for RankedCapture<T> {}
