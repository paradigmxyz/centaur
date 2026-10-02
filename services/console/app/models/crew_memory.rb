class CrewMemory < ApplicationRecord
  belongs_to :crew_profile

  validates :scope_key, format: { with: /\A(?:shared|[CDG][A-Z0-9]+)\z/ }
  validates :key, format: { with: /\A[a-z0-9][a-z0-9-]{0,63}\z/ }, uniqueness: { scope: [ :crew_profile_id, :scope_key ] }
  validates :content, presence: true
  validate :text_bounds

  scope :unexpired, -> { where("expires_at IS NULL OR expires_at > ?", Time.current) }

  def payload
    as_json(only: %i[key content source expires_at lock_version updated_at]).merge(
      "scope" => scope_key == "shared" ? "shared" : "conversation"
    )
  end

  private

  def text_bounds
    errors.add(:content, "must be at most 4 KiB") if content.to_s.bytesize > 4096
    errors.add(:source, "must be at most 1024 bytes") if source.to_s.bytesize > 1024
    if expires_at_before_type_cast.present? && expires_at.nil?
      errors.add(:expires_at, "must be a valid timestamp")
    end
  end
end
