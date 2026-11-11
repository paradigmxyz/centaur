# Roles slice. The pages live in the SPA (web/src/features/roles); Rails only
# keeps path helpers for redirects and links from legacy pages.
direct(:console_roles) { "/console/roles" }
direct(:console_role) { |oid| "/console/roles/#{oid}" }

# /api/v1 additions used by the role pages (see docs/API.md#roles).
namespace :api do
  namespace :v1 do
    resources :roles, only: [] do
      member do
        put "slack_channel_permissions", action: :replace_slack_channel_permissions
        delete "slack_channel_permissions/:channel_id", action: :destroy_slack_channel_permission,
               as: :slack_channel_permission, constraints: { channel_id: /[^\/]+/ }
      end
    end
  end
end
