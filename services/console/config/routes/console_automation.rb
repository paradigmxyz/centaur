# GET /api/v1/scheduled_tasks/:id is drawn in api.rb.
namespace :api do
  namespace :v1 do
    resources :workflows, only: %i[index show], constraints: { id: %r{[^/]+} }, format: false do
      member do
        get :runs
        post :runs, action: :create_run
      end
    end
    resources :scheduled_tasks, only: %i[index new edit create update destroy] do
      post :run, on: :member
      get :slack_channel_options, on: :collection
    end
  end
end
