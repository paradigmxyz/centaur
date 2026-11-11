# Secrets slice. The pages live in the SPA (web/src/features/secrets).

# /api/v1 additions used by the secrets pages (see docs/API.md#secrets).
namespace :api do
  namespace :v1 do
    get "secrets/:kind/:id", to: "secrets#show", as: :secret_detail
    scope "secrets/:kind/:secret_id", as: :secret do
      resources :role_grants, only: %i[index create destroy], controller: :secret_role_grants
    end
  end
end
