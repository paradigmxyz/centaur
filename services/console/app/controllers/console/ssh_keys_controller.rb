# Self-service management of the signed-in user's personal SSH key. The key is
# an ordinary SshKeySecret owned by the user; saving it grants it to the user's
# own principals (PersonalSshKeyGrants). Only an admin can grant it elsewhere.
class Console::SshKeysController < ApplicationController
  layout "console"
  before_action :load_key

  def show
    redirect_to edit_console_ssh_key_path unless @key.persisted?
    @grants = @key.grants.includes(:principal, :role).order(:id) if @key.persisted?
  end

  def edit
  end

  def validate_key
    response.headers["Cache-Control"] = "no-store"
    public_key = SshKey.public_key(params[:private_key].to_s)
    render json: { valid: true, message: "Valid private key. Ready to save.", fingerprint: SshKey.fingerprint(public_key) }
  rescue SshKey::Invalid => error
    render json: { valid: false, message: error.message }, status: :unprocessable_entity
  end

  def update
    attributes = params.require(:ssh_key).permit(:name, :private_key)
    @key.name = attributes[:name] if attributes.key?(:name)
    @key.private_key = attributes[:private_key] if attributes[:private_key].present?
    if @key.save
      # Idempotent: saving again also repairs grants a failed earlier
      # enrollment never created.
      PersonalSshKeyGrants.new.apply_for_user(current_user)
      redirect_to console_ssh_key_path, notice: "SSH key saved. Register the public key in your environments."
    else
      render :edit, status: :unprocessable_entity
    end
  end

  # Deleting the key removes every grant of it, so access ends at the proxy's
  # next authorization check.
  def destroy
    @key.destroy! if @key.persisted?
    redirect_to console_integrations_path, notice: "SSH key deleted. Remove its public key from your environments if it is no longer needed."
  end

  private

  def load_key
    @key = current_user.personal_ssh_key ||
      current_user.build_personal_ssh_key(name: "My SSH key", created_by: current_user)
  end
end
