//! `page`/`limit` query parameters and the `meta` of a list response:
//! `{"page":..,"limit":..,"total":..,"total_pages":..}`.
//!
//! `limit` defaults to 50 and is clamped to 1..=200, `page` defaults to 1
//! and is at least 1. Extract it next to the endpoint's own filter (serde
//! cannot `flatten` numeric query values, so use two `Query` extractors):
//!
//! ```ignore
//! async fn index(Query(filter): Query<Filter>, Query(page): Query<PageParams>) -> ApiResult {
//!     let page = page.resolve();
//!     // ... LIMIT page.limit() OFFSET page.offset()
//!     Ok(Json(json!({"data": rows, "meta": page.meta(total)})).into_response())
//! }
//! ```

use serde::{Deserialize, Serialize};

pub const DEFAULT_LIMIT: u32 = 50;
pub const MAX_LIMIT: u32 = 200;

/// The raw query parameters.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct PageParams {
    pub page: Option<u32>,
    pub limit: Option<u32>,
}

impl PageParams {
    pub fn resolve(self) -> Page {
        Page {
            page: self.page.unwrap_or(1).max(1),
            limit: self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    pub page: u32,
    pub limit: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Meta {
    pub page: u32,
    pub limit: u32,
    pub total: i64,
    pub total_pages: i64,
}

impl Page {
    pub fn limit(&self) -> i64 {
        i64::from(self.limit)
    }

    pub fn offset(&self) -> i64 {
        i64::from(self.page - 1) * i64::from(self.limit)
    }

    pub fn meta(&self, total: i64) -> Meta {
        Meta {
            page: self.page,
            limit: self.limit,
            total,
            total_pages: (total + self.limit() - 1) / self.limit(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_clamps_and_meta() {
        let page = PageParams::default().resolve();
        assert_eq!((page.page, page.limit, page.offset()), (1, 50, 0));
        let page = PageParams {
            page: Some(0),
            limit: Some(9999),
        }
        .resolve();
        assert_eq!((page.page, page.limit), (1, 200));
        let page = PageParams {
            page: Some(3),
            limit: Some(0),
        }
        .resolve();
        assert_eq!((page.limit, page.offset()), (1, 2));
        let page = PageParams {
            page: Some(2),
            limit: Some(10),
        }
        .resolve();
        assert_eq!(page.offset(), 10);
        assert_eq!(page.meta(25).total_pages, 3);
        assert_eq!(page.meta(0).total_pages, 0);
        assert_eq!(page.meta(20).total_pages, 2);
    }
}
