namespace :api do
  namespace :v1 do
    namespace :console do
      # Every grantable kind in one list, and one secret's detail view.
      resources :secrets, only: %i[index]
      get "secrets/:kind/:id", to: "secrets#show", as: :secret_detail
      scope "secrets/:kind/:secret_id", as: :secret do
        resources :role_grants, only: %i[index create destroy], controller: :secret_role_grants
      end
      # The kinds the console creates and edits.
      resources :static_secrets, only: %i[show create update destroy]
      resources :pg_dsn_secrets, only: %i[show create update destroy]
      resources :gcp_auth_secrets, only: %i[show create update destroy]
      resources :gcp_id_token_secrets, only: %i[show create update destroy]
    end
  end
end
