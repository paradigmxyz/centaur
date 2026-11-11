# Secrets slice. The pages live in the SPA (web/src/features/secrets); Rails
# only keeps path helpers for links and redirects from legacy pages.
direct(:console_secrets) { "/console/secrets" }
direct(:console_secret) do |kind, oid, options = {}|
  path = "/console/secrets/#{kind}/#{oid}"
  options[:anchor].present? ? "#{path}##{options[:anchor]}" : path
end

# /api/v1 additions used by the secrets pages (see docs/API.md#secrets).
namespace :api do
  namespace :v1 do
    get "secrets/:kind/:id", to: "secrets#show", as: :secret_detail
    scope "secrets/:kind/:secret_id", as: :secret do
      resources :role_grants, only: %i[index create destroy], controller: :secret_role_grants
    end
  end
end

# Personal SSH key pages (owned by the skills slice).
namespace :console do
  resource :ssh_key, only: %i[show edit update destroy] do
    post :validate_key
  end
end
