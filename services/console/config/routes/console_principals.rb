# Principals slice. The pages live in the SPA (web/src/features/principals);
# Rails only keeps path helpers for redirects and links from legacy pages.
direct(:console_principals) { "/console/principals" }
direct(:console_principal) { |oid| "/console/principals/#{oid}" }
direct(:console_new_principal) { "/console/principals/new" }

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
