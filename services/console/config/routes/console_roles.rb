# Roles slice (HTML console routes). The Slack channel permission delete
# route is shared with the principal page until both pages move to the SPA.

namespace :console do
  resources :roles, only: %i[index show new create edit update] do
    member do
      get "slack_channel_options", to: "slack_channel_options#index",
          defaults: { owner_type: "role" }, as: :slack_channel_options
      post "grants", to: "roles#grant_secret", as: :grant_secret
      delete "grants/:grant_id", to: "roles#revoke_grant", as: :revoke_grant
      patch "slack_channel_permissions", to: "roles#update_slack_channel_permissions",
            as: :slack_channel_permissions
    end
  end
  delete "slack_channel_permissions/:slack_channel_permission_id", to: "slack_channel_permissions#destroy",
         as: :slack_channel_permission
end
