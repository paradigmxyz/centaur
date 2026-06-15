# The console's shared key/value row editor: an ordered map of rows, each
# { key:, value: }, collapsed into a hash keyed by name. Rows with a blank key
# are dropped. Backs labels on every console form and token-endpoint headers on
# broker credentials.
module KvRowParams
  extend ActiveSupport::Concern

  private

  def label_params
    kv_rows(params[:labels])
  end

  def kv_rows(raw)
    (raw&.to_unsafe_h || {}).values.each_with_object({}) do |row, acc|
      key = row["key"].to_s.strip
      acc[key] = row["value"].to_s if key.present?
    end
  end
end
