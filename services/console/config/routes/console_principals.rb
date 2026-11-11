# Principals slice (HTML console routes). Removed when the slice moves to the SPA.

# Operator console (server-rendered HTML UI).
root "console#principals"
get "console/principals", to: "console#principals", as: :console_principals
namespace :console do
  get  "principals/new", to: "principals#new",    as: :new_principal
  post "principals",     to: "principals#create", as: :create_principal
end
get "console/principals/:id", to: "console#principal", as: :console_principal
# Role assignments and direct grants managed from the principal detail page. The
# extra /roles and /grants path segments keep these clear of the show route above
# and avoid clobbering the console_principal_path helper.
namespace :console do
  delete "principals/:id",                  to: "principals#destroy", as: :delete_principal
  get    "principals/:id/slack_channel_options", to: "slack_channel_options#index",
         defaults: { owner_type: "principal" }, as: :principal_slack_channel_options
  patch  "principals/:id/sandbox_access",   to: "principals#update_sandbox_access", as: :principal_sandbox_access
  patch  "principals/:id/slack_channel_permissions", to: "principals#update_slack_channel_permissions", as: :principal_slack_channel_permissions
  post   "principals/:id/roles",            to: "principals#assign_role",   as: :principal_assign_role
  delete "principals/:id/roles/:role_id",   to: "principals#unassign_role", as: :principal_unassign_role
  post   "principals/:id/grants",           to: "principals#grant_secret",  as: :principal_grant_secret
  delete "principals/:id/grants/:grant_id", to: "principals#revoke_grant",  as: :principal_revoke_grant
end

# /api/v1 additions used by the principals pages (see docs/API.md#principals).
namespace :api do
  namespace :v1 do
    resources :principals, only: %i[destroy] do
      member do
        get "effective_grants"
        put "slack_channel_permissions", action: :replace_slack_channel_permissions
        delete "slack_channel_permissions/:channel_id", action: :destroy_slack_channel_permission,
               as: :slack_channel_permission, constraints: { channel_id: /[^\/]+/ }
      end
    end
  end
end
