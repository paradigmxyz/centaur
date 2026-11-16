# All admin-only.
namespace :api do
  namespace :v1 do
    namespace :console do
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
end
