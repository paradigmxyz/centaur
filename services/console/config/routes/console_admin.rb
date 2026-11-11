# Admin slice: users, API clients, system settings, and descope. The pages live
# in the SPA (web/src/features/admin); Rails keeps path helpers for links from
# legacy pages (the Control tabs).
direct(:console_users) { "/console/users" }
direct(:console_api_clients) { "/console/api_clients" }
direct(:edit_console_system_settings) { "/console/settings/edit" }

# Admin self-descope ("view as operator") from the legacy layout's account menu
# and banner. Removed with the legacy layout at teardown; the SPA uses
# POST/DELETE /api/v1/descope.
namespace :console do
  resource :descope, only: %i[create destroy]
end

# /api/v1 additions used by the admin pages (see docs/API.md#users,
# #api-clients, and #system-settings). All admin-only.
namespace :api do
  namespace :v1 do
    resources :users, only: %i[index] do
      member do
        post :approve
        post :disable
        post :promote
        post :demote
      end
    end

    # Machine clients of the api-rs session API; 404 while the feature is off.
    resources :api_clients, only: %i[index show create update destroy] do
      member do
        post :rotate_secret
        post :disable
        post :enable
        post "principals", action: :attach_principal, as: :attach_principal
        delete "principals/:link_id", action: :detach_principal, as: :detach_principal
        post "principals/:link_id/default", action: :make_default_principal, as: :make_default_principal
      end
    end

    resource :system_settings, only: %i[show update] do
      get :slack_external_teams
      put :slack_external_teams, action: :replace_slack_external_teams
    end
  end
end
