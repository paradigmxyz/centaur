namespace :api do
  namespace :v1 do
    namespace :console do
      resources :roles, only: %i[index show create update destroy] do
        member do
          put "slack_channel_permissions", action: :replace_slack_channel_permissions
          delete "slack_channel_permissions/:channel_id", action: :destroy_slack_channel_permission,
                 as: :slack_channel_permission, constraints: { channel_id: /[^\/]+/ }
        end
        # Grants whose grantee is this role.
        resources :grants, only: %i[index], controller: :grantee_grants
      end
    end
  end
end
