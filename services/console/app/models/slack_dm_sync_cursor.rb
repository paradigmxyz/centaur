class SlackDmSyncCursor < ApplicationRecord
  validates :oauth_app_slug, presence: true, uniqueness: true
end
