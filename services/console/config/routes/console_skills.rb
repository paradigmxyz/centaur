# Skills slice: skill authoring and the personal SSH key. The pages live in the
# SPA (web/src/features/skills); Rails only keeps path helpers for redirects and
# links from legacy pages (e.g. the Integrations SSH card).
direct(:console_skills) { "/console/skills" }
direct(:mine_console_skills) { "/console/skills/mine" }
direct(:new_console_skill) { "/console/skills/new" }
direct(:console_skill) { |oid| "/console/skills/#{oid}" }
direct(:edit_console_skill) { |oid| "/console/skills/#{oid}/edit" }
direct(:console_ssh_key) { "/console/ssh_key" }
direct(:edit_console_ssh_key) { "/console/ssh_key/edit" }

# /api/v1 endpoints behind those pages (see docs/API.md#console-skills and
# docs/API.md#personal-ssh-key). Open to any active user.
namespace :api do
  namespace :v1 do
    resources :skills, only: %i[index show create update destroy] do
      collection do
        get :editor_candidates
      end
      member do
        post :share
        post :unshare
      end
    end

    scope "me", as: "me" do
      resource :ssh_key, only: %i[show update destroy], controller: "personal_ssh_keys" do
        post :validate
      end
    end
  end
end
