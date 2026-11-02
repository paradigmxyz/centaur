class Console::SshConnectionsController < ApplicationController
  layout "console"
  before_action :load_connection

  def show
    redirect_to edit_console_ssh_connection_path unless @connection.persisted?
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
    attributes = params.require(:ssh_connection).permit(:name, :private_key).to_h
    attributes.delete("private_key") if attributes["private_key"].blank?
    @connection.assign_attributes(attributes)
    @connection.expires_at = nil
    @connection.revoked_at = nil
    if @connection.save
      redirect_to console_ssh_connection_path, notice: "SSH key saved. Register the public key in your environments."
    else
      render :edit, status: :unprocessable_entity
    end
  end

  def destroy
    @connection.revoke! if @connection.persisted?
    redirect_to console_integrations_path, notice: "SSH disconnected. Remove the public key from your environments if it is no longer needed."
  end

  private

  def load_connection
    @connection = current_user.ssh_connection || current_user.build_ssh_connection(name: "My SSH key")
  end
end
