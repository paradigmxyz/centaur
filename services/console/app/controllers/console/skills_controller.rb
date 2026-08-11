class Console::SkillsController < ApplicationController
  layout "console"

  def index
    load_index("directory")
  end

  def mine
    load_index("mine")
    render :index
  end

  def show
    @skill = readable_skill
  end

  def new
    @skill = current_user.skills.new(content: "# Instructions")
  end

  def create
    @skill = current_user.skills.new(skill_params)
    if @skill.save
      redirect_to console_skill_path(@skill.oid), notice: "Skill created."
    else
      render :new, status: :unprocessable_entity
    end
  end

  def edit
    @skill = owned_skill
  end

  def update
    @skill = owned_skill
    if @skill.update(skill_params)
      redirect_to console_skill_path(@skill.oid), notice: "Skill saved."
    else
      render :edit, status: :unprocessable_entity
    end
  rescue ActiveRecord::StaleObjectError
    redirect_to edit_console_skill_path(params[:id]), alert: "This skill changed since you opened it. Review the latest content before saving."
  end

  def destroy
    owned_skill.archive!
    redirect_to mine_console_skills_path, notice: "Skill archived."
  end

  def share
    owned_skill.share!
    redirect_to console_skill_path(params[:id]), notice: "Skill is now shared."
  rescue ActiveRecord::RecordInvalid => e
    redirect_to console_skill_path(params[:id]), alert: e.record.errors.full_messages.to_sentence
  end

  def unshare
    skill = moderation_skill
    skill.unshare!
    redirect_to console_skill_path(skill.oid), notice: "Skill is now private."
  end

  private

  def load_index(tab)
    @tab = tab
    @query = params[:q].to_s.strip
    @skills = if @tab == "mine"
      current_user.skills.active
    else
      Skill.active.shared.includes(:user)
    end
    @skills = @skills.search(@query) if @query.present?
    @skills = @skills.order(updated_at: :desc, id: :asc)
  end

  def readable_skill
    Skill.catalog_visible_to(current_user).find_by_oid!(params[:id])
  end

  def owned_skill
    current_user.skills.active.find_by_oid!(params[:id])
  end

  def moderation_skill
    return owned_skill unless acting_admin?

    Skill.active.shared.find_by_oid!(params[:id])
  end

  def skill_params
    params.require(:skill).permit(:name, :description, :content, :lock_version)
  end
end
