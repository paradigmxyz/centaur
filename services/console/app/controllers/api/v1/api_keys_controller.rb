module Api
  module V1
    class ApiKeysController < Api::BaseController
      def index
        scope = current_user.api_keys.order(created_at: :asc, id: :asc)
        limit = pagination_limit
        page = pagination_page
        total = scope.count
        records = scope.limit(limit).offset((page - 1) * limit)
        total_pages = total.zero? ? 0 : ((total + limit - 1) / limit)
        render json: {
          data: records.map { |k| record_payload(k) },
          meta: { page: page, limit: limit, total: total, total_pages: total_pages }
        }
      end

      def show
        key = current_user.api_keys.find_by_oid!(params[:id])
        render json: { data: record_payload(key) }
      end

      def create
        attrs = data_params.permit(:name)
        key = current_user.api_keys.new(attrs)
        key.save!
        render status: :created, json: { data: record_payload(key).merge(token: key.token) }
      rescue ActiveRecord::RecordInvalid => e
        render_validation_error(e.record)
      end

      def destroy
        key = current_user.api_keys.find_by_oid!(params[:id])
        if key.id == current_api_key.id
          return render status: :unprocessable_entity,
                        json: { error: { message: "cannot revoke the API key used for this request" } }
        end
        key.soft_delete!
        head :no_content
      end

      private

      def record_payload(key)
        {
          id: key.oid,
          name: key.name,
          created_at: key.created_at,
          updated_at: key.updated_at
        }
      end
    end
  end
end
