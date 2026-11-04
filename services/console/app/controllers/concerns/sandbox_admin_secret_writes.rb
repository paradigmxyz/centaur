# Lets an admin's Slack DM sandbox create and edit a single-source secret
# without handling a secret value: a source may only reference an external
# store, never an inline (control_plane) value or an OAuth (token_broker)
# credential. Updates change only the supplied fields and keep the current
# source when none is given. Include into a subclass of the secret's Api::V1
# controller that defines secret_model, source_key, source_association, and
# current_document.
module SandboxAdminSecretWrites
  extend ActiveSupport::Concern
  include SandboxAdminAuthentication

  class Refused < StandardError; end

  REFUSED_SOURCE_TYPES = %w[control_plane token_broker].freeze

  included do
    rescue_from Refused do |error|
      render_error(status: :forbidden, message: error.message)
    end
  end

  # PATCH/PUT /:id updates an existing secret by OID.
  def update
    ref = secret_model.find_by_oid!(params[:id])
    source = ref.public_send(source_association)
    raise Refused, "OAuth credential secrets cannot be edited from a sandbox" if source&.source_type == "token_broker"

    document = current_document(ref)
    document[source_key.to_s] = source.slice("source_type", "config", "secret") if source
    assign_and_save!(ref, data_params.reverse_merge(document))
    render json: { data: record_payload(ref) }
  rescue ActiveRecord::RecordInvalid => e
    render_validation_error(e.record)
  end

  private

  def data_params
    data = super
    raise ActionController::BadRequest, "data must be an object" unless data.is_a?(ActionController::Parameters)

    source = data[source_key]
    if source.is_a?(ActionController::Parameters) && REFUSED_SOURCE_TYPES.include?(source[:source_type])
      raise Refused, "#{source[:source_type]} sources cannot be set from a sandbox; reference an external store"
    end

    data
  end
end
