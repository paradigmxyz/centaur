# Admin slice: users, API clients, system settings, and descope (HTML console
# routes).

# Operator (console user) management. Admin-only; pending users are approved here.
namespace :console do
  resources :users, only: %i[index] do
    member do
      post :approve
      post :disable
      post :promote
      post :demote
    end
  end
  # Machine clients of the api-rs session API. Admin-only.
  resources :api_clients do
    member do
      post :rotate_secret
      post :disable
      post :enable
      post "principals", action: :attach_principal, as: :attach_principal
      delete "principals/:link_id", action: :detach_principal, as: :detach_principal
      post "principals/:link_id/default", action: :make_default_principal, as: :make_default_principal
    end
  end
  resource :system_settings, only: %i[edit update], path: "settings" do
    patch :slack_external_teams
  end
  # Admin self-descope ("view as operator"): pause (admin-only) and restore
  # admin permissions. A singular resource because it's a per-session flag.
  resource :descope, only: %i[create destroy]
end
