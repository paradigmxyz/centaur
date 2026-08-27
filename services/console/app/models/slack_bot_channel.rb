class SlackBotChannel < ApplicationRecord
  scope :active, -> { where(active: true, archived: false) }
  scope :ordered, -> { order(Arel.sql("lower(name)"), :channel_id) }
  scope :for_team, ->(team_id) { team_id.present? ? where(team_id: team_id) : all }
  scope :excluding_channel_ids, lambda { |channel_ids|
    Array(channel_ids).any? ? where.not(channel_id: Array(channel_ids)) : all
  }
  scope :matching, ->(query) do
    needle = query.to_s.strip
    if needle.present?
      escaped = ActiveRecord::Base.sanitize_sql_like(needle)
      where("name ILIKE :query OR channel_id ILIKE :query", query: "%#{escaped}%")
    else
      all
    end
  end
  scope :with_members, ->(user_ids) do
    requested = Array(user_ids).map { |id| id.to_s.strip }.reject(&:blank?).uniq
    if requested.any?
      bot_user_id = limit(1).pick(:bot_user_id)
      bot_user_id ? where("member_user_ids @> ARRAY[?]::text[]", [ bot_user_id, *requested ].uniq) : none
    else
      all
    end
  end

  validates :team_id, :bot_user_id, :channel_id, :name, presence: true
  validates :channel_id, uniqueness: { scope: :team_id }
end
