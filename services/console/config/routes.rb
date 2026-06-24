Rails.application.routes.draw do
  # Define your application routes per the DSL in https://guides.rubyonrails.org/routing.html

  # Reveal health status on /up that returns 200 if the app boots with no exceptions, otherwise 500.
  # Can be used by load balancers and uptime monitors to verify that the app is live.
  get "up" => "rails/health#show", as: :rails_health_check

  # Render dynamic PWA files from app/views/pwa/* (remember to link manifest in application.html.erb)
  # get "manifest" => "rails/pwa#manifest", as: :pwa_manifest
  # get "service-worker" => "rails/pwa#service_worker", as: :pwa_service_worker

  # Operator console session login (cookie-based, separate from the API key auth).
  get "login", to: "sessions#new", as: :login
  post "login", to: "sessions#create"
  delete "logout", to: "sessions#destroy", as: :logout
  # Holding page for a signed-in user whose account is still pending approval.
  get "pending", to: "sessions#pending", as: :pending

  # Console SSO login, keyed by provider (/auth/google/start). Deliberately
  # unauthenticated; distinct /auth/* prefix avoids colliding with the broker's
  # /oauth/:slug/* consent flow below.
  get "auth/:provider/start", to: "session_oauth#start", as: :auth_start
  get "auth/:provider/callback", to: "session_oauth#callback", as: :auth_callback

  # Operator console (server-rendered HTML UI).
  root "console#principals"
  get "console/principals", to: "console#principals", as: :console_principals
  get "console/principals/:id", to: "console#principal", as: :console_principal
  namespace :console do
    resources :roles, only: %i[index show new create edit update] do
      member do
        post "grants", to: "roles#grant_secret", as: :grant_secret
        delete "grants/:grant_id", to: "roles#revoke_grant", as: :revoke_grant
      end
    end
  end
  # Role assignments and direct grants managed from the principal detail page. The
  # extra /roles and /grants path segments keep these clear of the show route above
  # and avoid clobbering the console_principal_path helper.
  namespace :console do
    post   "principals/:id/roles",            to: "principals#assign_role",   as: :principal_assign_role
    delete "principals/:id/roles/:role_id",   to: "principals#unassign_role", as: :principal_unassign_role
    post   "principals/:id/grants",           to: "principals#grant_secret",  as: :principal_grant_secret
    delete "principals/:id/grants/:grant_id", to: "principals#revoke_grant",  as: :principal_revoke_grant
  end
  get "console/secrets", to: "console#secrets", as: :console_secrets
  # One controller per secret kind for the create/edit forms. Declared before the
  # show route so their paths win over the generic `:kind/:id` match.
  namespace :console do
    resources :static_secrets, only: %i[new create edit update destroy], path: "secrets/static"
    resources :pg_dsn_secrets, only: %i[new create edit update destroy], path: "secrets/pg_dsn"
    resources :gcp_auth_secrets, only: %i[new create edit update destroy], path: "secrets/gcp_auth"
    post   "secrets/:kind/:id/roles",           to: "secrets#grant_role",        as: :secret_grant_role
    delete "secrets/:kind/:id/roles/:grant_id", to: "secrets#revoke_role_grant", as: :secret_revoke_role_grant
  end
  get "console/secrets/:kind/:id", to: "console#secret", as: :console_secret
  get "console/credentials", to: "console#credentials", as: :console_credentials
  # Create/edit form for broker credentials. Declared before the show route so
  # /console/credentials/new wins over the generic `:id` match.
  namespace :console do
    resources :broker_credentials, only: %i[new create edit update destroy], path: "credentials"
  end
  get "console/credentials/:id", to: "console#credential", as: :console_credential
  get "console/oauth_apps", to: "console#oauth_apps", as: :console_oauth_apps
  # Create/edit forms for OAuth apps. Declared before the show route so
  # /console/oauth_apps/new wins over the generic `:id` match. Named
  # `*_oauth_app_form*` so the form helpers don't collide with the read
  # (list/show) routes below, which keep the clean `console_oauth_app(s)` names.
  namespace :console do
    resources :oauth_apps, only: %i[new create edit update], as: :oauth_app_forms
  end
  get "console/oauth_apps/:id", to: "console#oauth_app", as: :console_oauth_app

  # Operator (console user) management. Admin-only; pending users are approved here.
  namespace :console do
    resources :users, only: %i[index] do
      member do
        post :approve
        post :disable
        post :promote
      end
    end
  end

  namespace :api do
    namespace :v1 do
      # Each secret type is addressable by opaque oid (member routes) or by an
      # explicit namespace + foreign_id via the namespaced lookup route.
      resources :static_secrets, only: %i[index show create update destroy] do
        collection { get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup }
      end
      resources :gcp_auth_secrets, only: %i[index show create update destroy] do
        collection { get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup }
      end
      resources :aws_auth_secrets, only: %i[index show create update destroy] do
        collection { get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup }
      end
      resources :oauth_token_secrets, only: %i[index show create update destroy] do
        collection { get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup }
      end
      resources :pg_dsn_secrets, only: %i[index show create update destroy] do
        collection { get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup }
      end
      resources :hmac_secrets, only: %i[index show create update destroy] do
        collection { get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup }
      end
      resources :roles, only: %i[index show create update destroy] do
        collection do
          get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup
        end
        # Grants whose grantee is this role. :role_id is the role's oid.
        resources :grants, only: %i[index], controller: :grantee_grants
      end
      resources :principals, only: %i[index show create update] do
        collection do
          get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup
          get "lookup/:namespace/:foreign_id/effective_config",
              action: :effective_config, as: :lookup_effective_config
        end
        member do
          get "effective_config"
        end
        # Role assignments for a principal. :id is the role's oid.
        resources :roles, only: %i[index create destroy], controller: :principal_roles
        # Grants whose grantee is this principal. :principal_id is the principal's oid.
        resources :grants, only: %i[index], controller: :grantee_grants
      end
      resources :grants, only: %i[show create destroy]
      resources :api_keys, only: %i[index show create destroy]
      resources :proxies, only: %i[index show create update destroy]

      # Operator-managed broker credentials (ApiKey auth). CRUD + lookup; the
      # rotating token blob is never serialized back.
      resources :broker_credentials, only: %i[index show create update destroy] do
        collection { get "lookup/:namespace/:foreign_id", action: :lookup, as: :lookup }
      end

      # Operator-managed OAuth apps (ApiKey auth). Addressed by oid or slug; CRUD
      # + lookup-by-slug. client_secret is write-only and never serialized back.
      resources :oauth_apps, only: %i[index show create update destroy] do
        collection { get "lookup/:slug", action: :lookup, as: :lookup }
      end

      # Called by iron-proxy instances (proxy bearer auth, not ApiKey auth).
      post "proxy/sync", to: "proxy_sync#create"
    end
  end

  # Public OAuth consent flow, keyed by the app's well-known slug
  # (/oauth/google/start). Deliberately unauthenticated: a team member clicks the
  # link to connect an integration; the provider is derived from the app.
  get "oauth/:slug/start", to: "oauth/flows#start", as: :oauth_start
  get "oauth/:slug/callback", to: "oauth/flows#callback", as: :oauth_callback

  # Render a JSON 404 for any unmatched route instead of the static error page.
  match "*path", to: "errors#not_found", via: :all
end
