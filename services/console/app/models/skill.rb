require "digest"
require "yaml"

class Skill < ApplicationRecord
  oid_prefix "skl"

  MAX_DOCUMENT_BYTES = 64.kilobytes
  NAME_FORMAT = /\A[a-z0-9]+(?:-[a-z0-9]+)*\z/
  RESERVED_NAMES = %w[search].freeze
  # Match any query term, like keyword search, instead of plainto_tsquery's
  # all-terms default. Lexemes never contain spaces, so only operators change.
  SEARCH_QUERY = "replace(plainto_tsquery('english', ?)::text, ' & ', ' | ')::tsquery"

  belongs_to :user
  has_many :skill_editors, dependent: :destroy
  has_many :editors, through: :skill_editors, source: :user

  enum :visibility, { private: "private", shared: "shared" },
       default: :shared, validate: true, prefix: true

  scope :active, -> { where(archived_at: nil) }
  scope :shared, -> { where(visibility: "shared") }
  scope :editable_by, lambda { |user|
    return none unless user

    active.where(user: user).or(
      active.where(id: SkillEditor.where(user: user).select(:skill_id))
    )
  }
  scope :catalog_visible_to, lambda { |user|
    shared_scope = active.shared
    user ? shared_scope.or(editable_by(user)) : shared_scope
  }

  normalizes :name, :description, with: ->(value) { value.to_s.strip }
  normalizes :content, with: ->(value) { value.to_s.gsub("\r\n", "\n").strip }
  before_validation :sync_shared_at

  validates :content, presence: true
  validates :name, presence: true, length: { maximum: 64 }, format: { with: NAME_FORMAT }
  validates :name, exclusion: { in: RESERVED_NAMES, message: "is reserved" }
  validates :description, presence: true
  validates :name, uniqueness: {
    conditions: -> { active },
    message: "is already used by another skill"
  }
  validate :document_size

  def self.search(query)
    normalized = query.to_s.strip
    return none if normalized.blank?

    # search_vector weights name, description, and content as A, B, and C.
    where("skills.search_vector @@ #{SEARCH_QUERY}", normalized)
      .order(Arel.sql(sanitize_sql_array([ "lower(skills.name) = lower(?) DESC", normalized ])))
      .order(Arel.sql(sanitize_sql_array([ "ts_rank(skills.search_vector, #{SEARCH_QUERY}) DESC", normalized ])))
      .order(updated_at: :desc, id: :asc)
  end

  def skill_document
    frontmatter = YAML.dump(
      "name" => name,
      "description" => description
    ).delete_prefix("---\n")

    "---\n#{frontmatter}---\n\n#{content}\n"
  end

  def share!
    update!(visibility: :shared, shared_at: shared_at || Time.current)
  end

  def shared?
    visibility_shared?
  end

  def editable_by?(candidate)
    candidate && (user_id == candidate.id || editors.exists?(candidate.id))
  end

  def unshare!
    update!(visibility: :private, shared_at: nil)
  end

  def archive!
    update!(archived_at: Time.current, visibility: :private, shared_at: nil)
  end

  def checksum
    Digest::SHA256.hexdigest(skill_document)
  end

  def catalog_payload(include_document: false)
    payload = {
      id: oid,
      name: name,
      description: description,
      visibility: visibility,
      author: user.name.presence || user.email,
      updated_at: updated_at&.iso8601,
      checksum: checksum
    }
    payload[:document] = skill_document if include_document
    payload
  end

  private

  def sync_shared_at
    self.shared_at = visibility_shared? ? (shared_at || Time.current) : nil
  end

  def document_size
    return if skill_document.bytesize <= MAX_DOCUMENT_BYTES

    errors.add(:content, "makes the complete SKILL.md exceed 64 KiB")
  end
end
