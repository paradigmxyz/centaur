# Secrets slice: secret list/detail, per-kind forms, role grants
# (HTML console routes). The personal SSH key lives in console_skills.rb.

get "console/secrets", to: "console#secrets", as: :console_secrets
# One controller per secret kind for the create/edit forms. Declared before the
# show route so their paths win over the generic `:kind/:id` match.
namespace :console do
  resources :static_secrets, only: %i[new create edit update destroy], path: "secrets/static"
  resources :pg_dsn_secrets, only: %i[new create edit update destroy], path: "secrets/pg_dsn"
  resources :gcp_auth_secrets, only: %i[new create edit update destroy], path: "secrets/gcp_auth"
  resources :gcp_id_token_secrets, only: %i[new create edit update destroy], path: "secrets/gcp_id_token"
  post   "secrets/:kind/:id/roles",           to: "secrets#grant_role",        as: :secret_grant_role
  delete "secrets/:kind/:id/roles/:grant_id", to: "secrets#revoke_role_grant", as: :secret_revoke_role_grant
end
get "console/secrets/:kind/:id", to: "console#secret", as: :console_secret
