module Console
  class PrincipalReconciliationsController < ApplicationController
    layout "console"

    def index
      @entries = reconciliation.entries
    end

    def apply
      result = reconciliation.apply(
        principal_oid: params[:principal_id],
        credential_oids: params[:credential_ids],
        current_user: current_user
      )
      redirect_to console_principal_reconciliation_path,
                  notice: "Granted #{result[:created]} of #{result[:requested]} requested credential tokens."
    rescue ActiveRecord::RecordNotFound => e
      redirect_to console_principal_reconciliation_path, alert: e.message
    rescue ActiveRecord::RecordInvalid => e
      redirect_to console_principal_reconciliation_path, alert: e.record.errors.full_messages.to_sentence
    end

    private

    def reconciliation
      @reconciliation ||= PrincipalCredentialReconciliation.new
    end
  end
end
