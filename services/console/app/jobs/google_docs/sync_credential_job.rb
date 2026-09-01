module GoogleDocs
  # Discard jobs enqueued before the paginated sync rollout.
  class SyncCredentialJob < ApplicationJob
    queue_as :default

    def perform(*)
      nil
    end
  end
end
