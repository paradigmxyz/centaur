class SlackBotChannel < ApplicationRecord
  scope :active, -> { where(active: true, archived: false) }
  scope :ordered, -> { order(Arel.sql("lower(name)"), :channel_id) }

  validates :configuration_digest, :team_id, :bot_user_id, :channel_id, :name, presence: true
  validates :channel_id, uniqueness: { scope: %i[configuration_digest team_id] }

  def catalog_channel
    SlackChannelCatalog::Channel.new(id: channel_id, name: name, private: private)
  end
end
