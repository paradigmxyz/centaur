# Path helper for the admin landing-page redirect.
direct(:console_principals) { "/console/principals" }

namespace :api do
  namespace :v1 do
    namespace :console do
      resources :principals, only: %i[index show create update destroy] do
        member do
          get "effective_grants"
          put "slack_channel_permissions", action: :replace_slack_channel_permissions
          delete "slack_channel_permissions/:channel_id", action: :destroy_slack_channel_permission,
                 as: :slack_channel_permission, constraints: { channel_id: /[^\/]+/ }
        end
        # Role assignments for a principal. :id is the role's oid.
        resources :roles, only: %i[index create destroy], controller: :principal_roles
        # Grants whose grantee is this principal.
        resources :grants, only: %i[index], controller: :grantee_grants
      end
    end
  end
end
