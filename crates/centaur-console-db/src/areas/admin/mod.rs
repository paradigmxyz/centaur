//! Console data access for the admin area: console users, API clients, system
//! settings with their default roles, API keys, and proxies.
//!
//! One submodule per Console resource. Each keeps the ordering, default scopes,
//! and write-time side effects its routes depend on; response shaping stays in
//! the API layer.

mod api_clients;
mod api_keys;
mod proxies;
mod system_settings;
mod users;

pub use api_clients::{
    ApiClientLink, ApiClientPrincipalRef, ApiClientRow, ApiClientWithLinks, api_client,
    api_clients, attach_api_client_principal, create_api_client, delete_api_client,
    detach_api_client_principal, find_api_client_link, find_api_client_principal,
    make_default_api_client_principal, rotate_api_client_secret, set_api_client_disabled,
    update_api_client,
};
pub use api_keys::{
    ApiKeyRow, KEY_LIST_FROM as API_KEY_LIST_FROM, KEY_LIST_ORDER as API_KEY_LIST_ORDER,
    KEY_LIST_SELECT as API_KEY_LIST_SELECT, api_key, create_api_key, soft_delete_api_key,
};
pub use proxies::{
    PROXY_LIST_FROM, PROXY_LIST_ORDER, PROXY_SELECT, ProxyChange, ProxyRow, admin_proxy,
    create_proxy, delete_proxy, update_proxy,
};
pub use system_settings::{
    RoleRow, SystemSettingRow, SystemSettingValues, existing_role_ids, replace_default_roles,
    roles, system_setting, write_system_settings,
};
pub use users::{
    ApproverRow, UserRow, admin_user, admin_users, approve_user, demote_user, disable_user,
    promote_user,
};
