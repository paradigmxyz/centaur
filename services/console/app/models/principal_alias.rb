# A retired foreign_id that still names the principal it was merged into, so
# callers that derive or stored the old id keep reaching the same principal.
class PrincipalAlias < ApplicationRecord
  oid_prefix "pral"

  belongs_to :principal

  validates :foreign_id, presence: true, uniqueness: true,
                         format: { with: Principal::URL_SAFE_FORMAT, message: Principal::URL_SAFE_MESSAGE }
  validate :foreign_id_is_not_a_principal

  private

  def foreign_id_is_not_a_principal
    errors.add(:foreign_id, :taken) if Principal.exists?(foreign_id: foreign_id)
  end
end
