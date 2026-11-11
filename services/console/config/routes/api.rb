# Existing /api/v1 resources. New SPA-facing endpoints live in the owning
# slice's config/routes/console_<slice>.rb inside its own
# `namespace :api do namespace :v1 do ... end end` block.

namespace :api do
  namespace :v1 do
    # Console SPA session endpoints (session cookie or bearer; see docs/API.md).
    resource :me, only: :show, controller: :me
    resource :descope, only: %i[create destroy]
    resource :session, only: :destroy
    # Each secret type is addressable by opaque oid or globally unique foreign_id.
    # The /lookup/default/... form is a temporary compatibility alias.
    resources :static_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :gcp_auth_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :gcp_id_token_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :aws_auth_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :oauth_token_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :pg_dsn_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :hmac_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :ssh_key_secrets, only: %i[index show create update destroy] do
      collection do
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end
    resources :roles, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
      member do
        post "slack_channel_permissions", action: :upsert_slack_channel_permission
      end
      # Grants whose grantee is this role. :role_id is the role's oid.
      resources :grants, only: %i[index], controller: :grantee_grants
    end
    resources :principals, only: %i[index show create update] do
      collection do
        get "lookup/default/:foreign_id/effective_config",
            action: :effective_config, as: :default_lookup_effective_config,
            constraints: { foreign_id: /[^\/]+/ }
        get "lookup/:foreign_id/effective_config", action: :effective_config, as: :lookup_effective_config,
            constraints: { foreign_id: /[^\/]+/ }
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup,
            constraints: { foreign_id: /[^\/]+/ }
        get "lookup/:foreign_id", action: :lookup, as: :lookup,
            constraints: { foreign_id: /[^\/]+/ }
      end
      member do
        get "effective_config"
        post "slack_channel_permissions", action: :upsert_slack_channel_permission
      end
      # Role assignments for a principal. :id is the role's oid.
      resources :roles, only: %i[index create destroy], controller: :principal_roles
      # Grants whose grantee is this principal. :principal_id is the principal's oid.
      resources :grants, only: %i[index], controller: :grantee_grants
    end
    resources :grants, only: %i[show create destroy]
    resources :api_keys, only: %i[index show create destroy]
    resources :proxies, only: %i[index show create update destroy]
    resources :scheduled_tasks, only: %i[show]
    # Operator-managed broker credentials (ApiKey auth). CRUD + lookup; the
    # rotating token blob is never serialized back.
    resources :broker_credentials, only: %i[index show create update destroy] do
      collection do
        get "lookup/default/:foreign_id", action: :lookup, as: :default_lookup
        get "lookup/:foreign_id", action: :lookup, as: :lookup
      end
    end

    # Operator-managed OAuth apps (ApiKey auth). Addressed by oid or slug; CRUD
    # + lookup-by-slug. client_secret is write-only and never serialized back.
    resources :oauth_apps, only: %i[index show create update destroy] do
      collection { get "lookup/:slug", action: :lookup, as: :lookup }
    end

    # Called from inside sandboxes through their assigned iron-proxy. The
    # proxy injects a short-lived sandbox entitlement JWT scoped to these paths.
    namespace :sandbox do
      resource :permissions, only: :show
      resources :oauth_apps, only: :index
      resources :scheduled_tasks, only: %i[index show create update destroy] do
        post :run, on: :member
      end
      resources :skills, only: %i[index show create update destroy] do
        collection { get :search }
        member do
          post :share
          post :unshare
          get :editors
          post :add_editor, path: "editors"
          delete :remove_editor, path: "editors"
        end
      end
      # Permission management from an active Console admin's Slack DM.
      namespace :admin do
        resources :principals, only: %i[index show create] do
          collection do
            get "lookup/:foreign_id", action: :lookup, as: :lookup, constraints: { foreign_id: /[^\/]+/ }
          end
          member do
            get "effective_grants", action: :effective_grants
            patch "sandbox_access", action: :update_sandbox_access
            post "slack_channel_permissions", action: :upsert_slack_channel_permission
            delete "slack_channel_permissions/:channel_id", action: :destroy_slack_channel_permission
          end
          resources :roles, only: %i[index create destroy], controller: :principal_roles
          resources :grants, only: %i[index], controller: :grantee_grants
        end
        resources :grants, only: %i[create destroy]
        resources :secrets, only: %i[index]
        resources :static_secrets, only: %i[show create update]
        resources :pg_dsn_secrets, only: %i[show create update]
        resources :gcp_id_token_secrets, only: %i[show create update]
        resources :slack_external_teams, only: %i[index create destroy]
        resources :roles, only: %i[index show] do
          collection { get "lookup/:foreign_id", action: :lookup, as: :lookup, constraints: { foreign_id: /[^\/]+/ } }
          resources :grants, only: %i[index], controller: :grantee_grants
        end
      end
    end
  end
end
