# Saves a skill written from Console and, when the caller may manage editors,
# replaces its editor list in the same transaction.
#
#   authoring = SkillAuthoring.new(skill, editor_ids: ["usr_…"]) # nil keeps editors
#   authoring.save # => true, or false with skill.errors populated
#
# Editors are resolved by user OID among active users other than the owner;
# any unknown, inactive, duplicate-of-owner, or malformed id fails validation
# with "Editors include an unavailable user" instead of being dropped.
# Changing the editor list bumps updated_at so catalog readers see the change.
class SkillAuthoring
  attr_reader :skill

  def initialize(skill, editor_ids: nil)
    @skill = skill
    @editor_ids = editor_ids
  end

  def editors
    @editors ||= manage_editors? ? resolve_editors : []
  end

  # Raises ActiveRecord::StaleObjectError on a lock_version conflict.
  def save
    selected = editors
    return false unless skill.valid?

    if @editor_selection_invalid
      skill.errors.add(:editors, "include an unavailable user")
      return false
    end

    if manage_editors?
      selected_ids = selected.map(&:id)
      skill.updated_at = Time.current if skill.editor_ids.sort != selected_ids.sort
    end

    Skill.transaction do
      skill.save!
      skill.editor_ids = selected.map(&:id) if manage_editors?
    end
    true
  end

  private

  def manage_editors?
    !@editor_ids.nil?
  end

  def resolve_editors
    submitted = Array(@editor_ids).map(&:to_s).compact_blank.uniq
    ids = submitted.filter_map { |oid| User.decode_oid(oid) }
    users_by_id = User.active.where(id: ids).index_by(&:id)
    users = ids.filter_map { |id| users_by_id[id] }.reject { |user| user.id == skill.user_id }

    @editor_selection_invalid = ids.length != submitted.length || users.length != ids.length
    users
  end
end
