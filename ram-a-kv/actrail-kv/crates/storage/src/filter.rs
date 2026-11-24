//! 本文件定义跨存储后端一致的 session 和采集时间筛选。

use time::{
    format_description::well_known::Rfc3339, macros::format_description, Date, OffsetDateTime,
    UtcOffset,
};

use crate::StorageError;

#[derive(Clone, Debug, Default)]
pub struct CaptureFilter {
    session_key: Option<String>,
    from: Option<OffsetDateTime>,
    until: Option<OffsetDateTime>,
}

impl CaptureFilter {
    pub fn new(
        session_key: Option<String>,
        from: Option<OffsetDateTime>,
        until: Option<OffsetDateTime>,
    ) -> Result<Self, StorageError> {
        if matches!((from, until), (Some(from), Some(until)) if from >= until) {
            return Err(StorageError::InvalidFilter(
                "from must precede until".into(),
            ));
        }
        Ok(Self {
            session_key,
            from,
            until,
        })
    }

    /// A date denotes the start of that day in UTC. Timestamps accept RFC 3339 offsets.
    pub fn parse(
        session_key: Option<String>,
        from: Option<&str>,
        until: Option<&str>,
    ) -> Result<Self, StorageError> {
        Self::new(
            session_key,
            from.map(parse_bound).transpose()?,
            until.map(parse_bound).transpose()?,
        )
    }

    pub fn session_key(&self) -> Option<&str> {
        self.session_key.as_deref()
    }

    pub fn from(&self) -> Option<OffsetDateTime> {
        self.from
    }

    pub fn until(&self) -> Option<OffsetDateTime> {
        self.until
    }

    pub(crate) fn start(&self, retention_cutoff: Option<OffsetDateTime>) -> Option<OffsetDateTime> {
        self.from.max(retention_cutoff)
    }
}

fn parse_bound(value: &str) -> Result<OffsetDateTime, StorageError> {
    Date::parse(value, format_description!("[year]-[month]-[day]"))
        .map(|date| date.midnight().assume_utc())
        .or_else(|_| OffsetDateTime::parse(value, &Rfc3339))
        .map_err(|_| {
            StorageError::InvalidFilter(format!(
                "{value:?} must be a UTC date or RFC 3339 timestamp"
            ))
        })
}

pub(crate) fn capture_time(value: &str) -> Result<OffsetDateTime, StorageError> {
    OffsetDateTime::parse(value, &Rfc3339).map_err(|_| StorageError::InvalidTime(value.into()))
}

pub(crate) fn time_key(value: OffsetDateTime) -> Result<String, StorageError> {
    if let Some(utc) = value.checked_to_offset(UtcOffset::UTC) {
        if (0..=9999).contains(&utc.year()) {
            return utc
                .format(format_description!(
                    "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:9]Z"
                ))
                .map_err(|_| StorageError::InvalidTime(value.to_string()));
        }
    }
    Err(StorageError::InvalidTime(value.to_string()))
}
