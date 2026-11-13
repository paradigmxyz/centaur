namespace :api do
  namespace :v1 do
    namespace :console do
      resources :broker_credentials, only: %i[index show create update destroy]
    end
  end
end
