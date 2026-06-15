module ForeignIdCollisionGuard
  extend ActiveSupport::Concern

  included do
    validate :foreign_id_does_not_shadow_oid
  end

  private

  # A foreign_id that began with this model's opaque-id prefix would be
  # ambiguous with a real oid: write endpoints accept either an oid or a
  # foreign_id in the same position, so reserving the prefix keeps them
  # distinguishable.
  def foreign_id_does_not_shadow_oid
    return if foreign_id.blank?
    reserved = "#{self.class.oid_prefix}_"
    return unless foreign_id.start_with?(reserved)
    errors.add(:foreign_id, "must not start with #{reserved.inspect}, which is reserved for opaque ids")
  end
end
