# Skills slice (HTML console routes).

namespace :console do
  resources :skills do
    collection do
      get :mine
    end
    member do
      post :share
      post :unshare
    end
  end
end
