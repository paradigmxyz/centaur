//! Console's JSON rendering of `datetime` columns: UTC RFC 3339 with exactly
//! three fractional digits.
//!
//! Console stores every timestamp as `timestamp without time zone` in UTC, so
//! rows usually carry [`time::PrimitiveDateTime`]; a row that went through
//! `now()` or a comparison may carry [`time::OffsetDateTime`]. Both, and their
//! `Option` forms, serialize through this module:
//!
//! ```ignore
//! use time::{OffsetDateTime, PrimitiveDateTime};
//!
//! #[derive(serde::Serialize)]
//! struct ApiKey {
//!     #[serde(with = "centaur_console_db::timestamp::primitive")]
//!     created_at: PrimitiveDateTime,
//!     #[serde(with = "centaur_console_db::timestamp::primitive_option")]
//!     deleted_at: Option<PrimitiveDateTime>,
//!     #[serde(with = "centaur_console_db::timestamp")]
//!     synced_at: OffsetDateTime,
//!     #[serde(with = "centaur_console_db::timestamp::option")]
//!     expires_at: Option<OffsetDateTime>,
//! }
//! ```
//!
//! Response structs that are built by hand use [`json`] and its variants
//! instead; both paths produce the same string.

use serde::{Deserialize, Deserializer, Serializer};
use serde_json::Value;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

/// Console's `datetime` rendering: `2024-01-02T03:04:05.678Z`.
pub fn format(value: OffsetDateTime) -> String {
    format_primitive(datetime(value))
}

/// The same rendering for a `timestamp without time zone` column, which
/// Console stores in UTC.
pub fn format_primitive(value: PrimitiveDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        value.year(),
        value.month() as u8,
        value.day(),
        value.hour(),
        value.minute(),
        value.second(),
        value.millisecond()
    )
}

pub fn json(value: OffsetDateTime) -> Value {
    Value::String(format(value))
}

pub fn json_option(value: Option<OffsetDateTime>) -> Value {
    value.map_or(Value::Null, json)
}

pub fn json_primitive(value: PrimitiveDateTime) -> Value {
    Value::String(format_primitive(value))
}

pub fn json_primitive_option(value: Option<PrimitiveDateTime>) -> Value {
    value.map_or(Value::Null, json_primitive)
}

/// The current UTC time as the `timestamp without time zone` value Console's
/// columns store.
pub fn now() -> PrimitiveDateTime {
    datetime(OffsetDateTime::now_utc())
}

/// An `OffsetDateTime` as the `timestamp without time zone` value Console's
/// columns store.
pub fn datetime(value: OffsetDateTime) -> PrimitiveDateTime {
    let value = value.to_offset(UtcOffset::UTC);
    PrimitiveDateTime::new(value.date(), value.time())
}

/// A stored `timestamp without time zone` value as UTC.
pub fn offset(value: PrimitiveDateTime) -> OffsetDateTime {
    value.assume_utc()
}

/// A `chrono` UTC time (cron schedules use chrono) as an `OffsetDateTime`.
pub fn from_chrono(value: chrono::DateTime<chrono::Utc>) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp_nanos(i128::from(
        value.timestamp_nanos_opt().unwrap_or_default(),
    ))
    .unwrap_or(OffsetDateTime::UNIX_EPOCH)
}

pub fn to_chrono(value: OffsetDateTime) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(value.unix_timestamp(), value.nanosecond()).unwrap_or_default()
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    OffsetDateTime::parse(&raw, &time::format_description::well_known::Rfc3339)
        .map_err(serde::de::Error::custom)
}

pub fn deserialize_primitive<'de, D>(deserializer: D) -> Result<PrimitiveDateTime, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(datetime(deserialize(deserializer)?))
}

pub fn serialize<S>(value: &OffsetDateTime, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format(*value))
}

pub fn serialize_primitive<S>(value: &PrimitiveDateTime, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format_primitive(*value))
}

pub fn serialize_option<S>(value: &Option<OffsetDateTime>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serialize(value, serializer),
        None => serializer.serialize_none(),
    }
}

pub fn serialize_primitive_option<S>(
    value: &Option<PrimitiveDateTime>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serialize_primitive(value, serializer),
        None => serializer.serialize_none(),
    }
}

pub fn deserialize_option<'de, D>(deserializer: D) -> Result<Option<OffsetDateTime>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer)?
        .map(|raw| {
            OffsetDateTime::parse(&raw, &time::format_description::well_known::Rfc3339)
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}

pub fn deserialize_primitive_option<'de, D>(
    deserializer: D,
) -> Result<Option<PrimitiveDateTime>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(deserialize_option(deserializer)?.map(datetime))
}

/// `#[serde(with = "centaur_console_db::timestamp::option")]`.
pub mod option {
    pub use super::{deserialize_option as deserialize, serialize_option as serialize};
}

/// `#[serde(with = "centaur_console_db::timestamp::primitive")]`.
pub mod primitive {
    pub use super::{deserialize_primitive as deserialize, serialize_primitive as serialize};
}

/// [`primitive`] for an `Option<PrimitiveDateTime>`.
pub mod primitive_option {
    pub use super::{
        deserialize_primitive_option as deserialize, serialize_primitive_option as serialize,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde_json::json;
    use time::macros::datetime;

    #[test]
    fn renders_rfc3339_with_milliseconds() {
        let value = OffsetDateTime::from_unix_timestamp_nanos(1_704_164_645_678_000_000).unwrap();
        assert_eq!(format(value), "2024-01-02T03:04:05.678Z");
        assert_eq!(json(value), json!("2024-01-02T03:04:05.678Z"));
        assert_eq!(
            format_primitive(datetime!(2024-03-04 05:06:07.008)),
            "2024-03-04T05:06:07.008Z"
        );
        assert_eq!(
            json_primitive(datetime!(2024-03-04 05:06:07)),
            json!("2024-03-04T05:06:07.000Z")
        );
        assert_eq!(json_option(None), Value::Null);
        assert_eq!(json_primitive_option(None), Value::Null);
    }

    #[test]
    fn a_non_utc_offset_is_rendered_as_utc() {
        let value = datetime!(2024-01-02 03:04:05.678 +02:00);
        assert_eq!(format(value), "2024-01-02T01:04:05.678Z");
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Timestamps {
        #[serde(with = "super")]
        created_at: OffsetDateTime,
        #[serde(with = "super::option")]
        deleted_at: Option<OffsetDateTime>,
        #[serde(with = "super::primitive")]
        updated_at: PrimitiveDateTime,
        #[serde(with = "super::primitive_option")]
        last_run_at: Option<PrimitiveDateTime>,
    }

    #[test]
    fn serde_with_round_trips_both_kinds_and_options() {
        let record = Timestamps {
            created_at: datetime!(2024-01-02 03:04:05.678 UTC),
            deleted_at: None,
            updated_at: datetime!(2024-01-02 03:04:05.678),
            last_run_at: Some(datetime!(2024-01-02 03:04:05.678)),
        };
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(
            value,
            json!({
                "created_at": "2024-01-02T03:04:05.678Z",
                "deleted_at": null,
                "updated_at": "2024-01-02T03:04:05.678Z",
                "last_run_at": "2024-01-02T03:04:05.678Z",
            })
        );
        assert_eq!(serde_json::from_value::<Timestamps>(value).unwrap(), record);
        assert!(
            serde_json::from_value::<Timestamps>(json!({
                "created_at": "nope",
                "deleted_at": null,
                "updated_at": "nope",
                "last_run_at": null,
            }))
            .is_err()
        );
    }
}
