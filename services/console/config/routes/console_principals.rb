# Path helper for the admin landing-page redirect.
direct(:console_principals) { "/console/principals" }

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
