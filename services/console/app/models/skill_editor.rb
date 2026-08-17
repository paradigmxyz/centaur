class SkillEditor < ApplicationRecord
  belongs_to :skill
  belongs_to :user

  validates :user_id, uniqueness: { scope: :skill_id }
  validate :editor_is_not_owner

  private

  def editor_is_not_owner
    return unless skill && user_id == skill.user_id

    errors.add(:user, "is already the skill owner")
  end
end
