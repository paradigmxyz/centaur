//! Small helpers on standard crates that areas share: CIDR parsing
//! (`ipnet`), JSON Schema validation (`jsonschema`), and Markdown to
//! sanitized HTML (`pulldown-cmark` + `ammonia`). SSH keys live in
//! `centaur_console_db::keys`, schedules in `centaur_console_db::schedule`.

pub mod cidr;
pub mod json_schema;
pub mod markdown;
