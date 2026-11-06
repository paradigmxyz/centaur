# A foreign id, beyond the principal's own, that names the same user: the
# Slack user or Console account they are also known by, or the foreign id of a
# principal merged into theirs. Lookups by any of them reach the one principal.
class PrincipalIdentity < ApplicationRecord
  oid_prefix "prid"

  belongs_to :principal

  validates :foreign_id, presence: true, uniqueness: true,
                         format: { with: Principal::URL_SAFE_FORMAT, message: Principal::URL_SAFE_MESSAGE }
  validate :foreign_id_is_not_a_principal

  private

  def foreign_id_is_not_a_principal
    errors.add(:foreign_id, :taken) if Principal.exists?(foreign_id: foreign_id)
  end
end
