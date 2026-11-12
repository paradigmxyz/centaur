module Api
  module V1
    # Console skill authoring for any active user (the SPA's Skills pages).
    #
    # Visibility follows the catalog: shared skills, the caller's own skills,
    # and skills the caller edits. Anything else is a 404. Owners and editors
    # update content; only the owner manages editors, visibility, and
    # archiving. An admin may additionally make any shared skill
    # private (moderation). Admins otherwise get no extra access here.
    class SkillsController < Api::BaseController
      accepts_credentials :session

      STALE_MESSAGE = "This skill changed since you opened it. Review the latest content before saving.".freeze

      allow_active_users

      def index
        tab = params[:tab].to_s == "mine" ? "mine" : "directory"
        skills = tab == "mine" ? Skill.editable_by(current_user) : Skill.active.shared
        query = params[:q].to_s.strip
        skills = skills.search(query) if query.present?
        skills = skills.includes(:user).order(updated_at: :desc, id: :asc)
        render json: { data: skills.map { |skill| summary_payload(skill) } }
      end

      def show
        render json: { data: detail_payload(Skill.catalog_visible_to(current_user).find_by_oid!(params[:id])) }
      end

      def create
        skill = current_user.skills.new(skill_attributes.except(:lock_version))
        authoring = SkillAuthoring.new(skill, editor_ids: submitted_editor_ids || [])
        return render_skill_errors(skill) unless authoring.save

        render json: { data: detail_payload(skill) }, status: :created
      end

      def update
        skill = Skill.editable_by(current_user).find_by_oid!(params[:id])
        skill.assign_attributes(skill_attributes)
        editor_ids = owner?(skill) ? submitted_editor_ids : nil
        return render_skill_errors(skill) unless SkillAuthoring.new(skill, editor_ids: editor_ids).save

        render json: { data: detail_payload(skill) }
      rescue ActiveRecord::StaleObjectError
        render_error(status: :conflict, message: STALE_MESSAGE, details: { reason: "stale" })
      end

      # Archives the skill (owner only); its name becomes reusable.
      def destroy
        owned_skill.archive!
        head :no_content
      end

      def share
        skill = owned_skill
        skill.share!
        render json: { data: detail_payload(skill) }
      rescue ActiveRecord::RecordInvalid => e
        render_skill_errors(e.record)
      end

      def unshare
        skill = acting_admin? ? Skill.active.shared.or(current_user.skills.active).find_by_oid!(params[:id]) : owned_skill
        skill.unshare!
        render json: { data: detail_payload(skill) }
      end

      # Active users the caller can add as editors of their own skills.
      def editor_candidates
        users = User.active.where.not(id: current_user.id).order(:email)
        query = params[:q].to_s.strip
        if query.present?
          pattern = "%#{User.sanitize_sql_like(query.downcase)}%"
          users = users.where("lower(users.email) LIKE :q OR lower(coalesce(users.name, '')) LIKE :q", q: pattern)
        end
        render json: { data: users.map { |user| user_payload(user) } }
      end

      private

      def owned_skill
        current_user.skills.active.find_by_oid!(params[:id])
      end

      def owner?(skill)
        skill.user_id == current_user.id
      end

      def skill_attributes
        submitted = params.require(:data).permit(:name, :description, :instructions, :lock_version)
        submitted[:content] = submitted.delete(:instructions) if submitted.key?(:instructions)
        submitted
      end

      # nil when the body omits editor_ids (keep the current editors).
      def submitted_editor_ids
        data = params.require(:data)
        return nil unless data.key?(:editor_ids)

        Array(data.permit(editor_ids: [])[:editor_ids])
      end

      def render_skill_errors(skill)
        details = skill.errors.as_json
        details[:instructions] = details.delete(:content) if details.key?(:content)
        render_error(status: :unprocessable_entity, message: "validation failed", details: details)
      end

      def summary_payload(skill)
        {
          id: skill.oid,
          name: skill.name,
          description: skill.description,
          visibility: skill.visibility,
          author: skill.user.name.presence || skill.user.email,
          owner: user_payload(skill.user),
          updated_at: skill.updated_at.iso8601,
          viewer: { owner: owner?(skill) }
        }
      end

      def detail_payload(skill)
        editors = skill.editors.active.order(:email).to_a
        owner = owner?(skill)
        summary_payload(skill).merge(
          instructions: skill.content,
          content_html: SkillMarkdown.render(skill.content),
          lock_version: skill.lock_version,
          checksum: skill.checksum,
          created_at: skill.created_at.iso8601,
          editors: editors.map { |editor| user_payload(editor) },
          viewer: {
            owner: owner,
            can_edit: owner || skill.editors.exists?(current_user.id),
            can_manage: owner,
            can_unshare: skill.shared? && (owner || acting_admin?)
          }
        )
      end

      def user_payload(user)
        { id: user.oid, name: user.name.presence, email: user.email }
      end
    end
  end
end
