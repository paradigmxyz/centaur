# The console API: /api/v1/console/*, Api::V1::Console controllers, console
# session only (see ConsoleApiAuthentication). Everything the SPA calls lives
# under this prefix, drawn here (shell and shared pickers) and in the area
# files config/routes/console_<area>.rb.
namespace :api do
  namespace :v1 do
    namespace :console do
      resource :me, only: :show, controller: :me
      resource :session, only: :destroy
      # Grants of any secret to a principal or role (grant pickers).
      resources :grants, only: %i[show create destroy]
      resources :slack_channels, only: %i[index]
    end
  end
end
