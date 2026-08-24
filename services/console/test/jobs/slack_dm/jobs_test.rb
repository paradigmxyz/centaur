require "test_helper"

module SlackDm
  class JobsTest < ActiveJob::TestCase
    def slack_app(slug: "slack-dms")
      OauthApp.create!(
        provider: "slack",
        slug: slug,
        client_id: "slack-client-#{SecureRandom.hex(4)}",
        client_secret: "secret",
        allowed_scopes: SlackDm::SyncCredential::REQUIRED_SCOPES,
        created_by: users(:acme_admin)
      )
    end

    def slack_credential(
      app:,
      scopes: SlackDm::SyncCredential::REQUIRED_SCOPES,
      access_token: "xoxp-live",
      provider_subject: "U#{SecureRandom.hex(4).upcase}"
    )
      BrokerCredential.create!(
        oauth_app: app,
        foreign_id: "slack-dms-#{SecureRandom.hex(6)}",
        token_endpoint: "https://slack.com/api/oauth.v2.access",
        access_token: access_token,
        refresh_token: "refresh",
        last_refresh: Time.current,
        expires_at: 1.hour.from_now,
        scopes: scopes,
        provider_subject: provider_subject
      )
    end

    test "SyncCredentialJob syncs eligible credentials serially" do
      app = slack_app
      good = slack_credential(app: app)
      dm_only = slack_credential(app: app, scopes: SlackDm::SyncCredential::DM_REQUIRED_SCOPES)
      slack_credential(app: app, scopes: %w[chat:write])
      slack_credential(app: app, access_token: nil)
      other_app = slack_app(slug: "other-slack")
      slack_credential(app: other_app)
      synced_ids = []
      sync_factory = lambda do |credential|
        Object.new.tap do |sync|
          sync.define_singleton_method(:call) do |**|
            synced_ids << credential.id
            true
          end
        end
      end

      SlackDm::SyncCredential.stub(:new, sync_factory) do
        assert_no_enqueued_jobs { SlackDm::SyncCredentialJob.perform_now("slack-dms") }
      end

      assert_equal [ good.id, dm_only.id ], synced_ids
      cursor = SlackDmSyncCursor.find_by!(oauth_app_slug: "slack-dms")
      assert_equal good.id, cursor.next_credential_id
      assert_nil cursor.not_before
    end

    test "SyncCredentialJob resumes from the persisted credential cursor" do
      app = slack_app
      first = slack_credential(app: app)
      second = slack_credential(app: app)
      third = slack_credential(app: app)
      SlackDmSyncCursor.create!(oauth_app_slug: "slack-dms", next_credential_id: second.id)
      synced_ids = []
      sync_factory = lambda do |credential|
        Object.new.tap do |sync|
          sync.define_singleton_method(:call) do |**|
            synced_ids << credential.id
            true
          end
        end
      end

      SlackDm::SyncCredential.stub(:new, sync_factory) do
        SlackDm::SyncCredentialJob.perform_now("slack-dms")
      end

      assert_equal [ second.id, third.id, first.id ], synced_ids
      assert_equal second.id,
                   SlackDmSyncCursor.find_by!(oauth_app_slug: "slack-dms").next_credential_id
    end

    test "SyncCredentialJob configures duplicate global runs to be discarded" do
      first = SlackDm::SyncCredentialJob.new("slack-dms")
      duplicate = SlackDm::SyncCredentialJob.new("slack-dms")
      other_app = SlackDm::SyncCredentialJob.new("other-slack")
      legacy_poll = SlackDm::PollSyncJob.new("slack-dms")

      assert_equal :discard, SlackDm::SyncCredentialJob.concurrency_on_conflict
      assert_equal 6.hours, SlackDm::SyncCredentialJob.concurrency_duration
      assert_equal 30.minutes, SlackDm::SyncCredentialJob::RUN_TIME_BUDGET
      assert_equal first.concurrency_key, duplicate.concurrency_key
      assert_equal first.concurrency_key, other_app.concurrency_key
      assert_equal first.concurrency_key, legacy_poll.concurrency_key
    end

    test "SyncCredentialJob ignores credential IDs queued by the previous job shape" do
      assert_no_difference -> { SlackDmSyncCursor.count } do
        SlackDm::SyncCredentialJob.perform_now(123)
      end
    end

    test "SyncCredentialJob continues after one credential has a Slack API error" do
      app = slack_app
      first = slack_credential(app: app)
      second = slack_credential(app: app)
      attempted_ids = []
      sync_factory = lambda do |credential|
        Object.new.tap do |sync|
          sync.define_singleton_method(:call) do |**|
            attempted_ids << credential.id
            raise SlackApi::Error, "invalid_auth" if credential == first

            true
          end
        end
      end

      SlackDm::SyncCredential.stub(:new, sync_factory) do
        SlackDm::SyncCredentialJob.perform_now("slack-dms")
      end

      assert_equal [ first.id, second.id ], attempted_ids
    end

    test "SyncCredentialJob pauses the cursor until Slack Retry-After elapses" do
      app = slack_app
      first = slack_credential(app: app)
      second = slack_credential(app: app)
      attempted_ids = []
      rate_limited = true
      sync_factory = lambda do |credential|
        Object.new.tap do |sync|
          sync.define_singleton_method(:call) do |**_kwargs, &checkpoint|
            attempted_ids << credential.id
            if credential == first && rate_limited
              checkpoint.call("D200")
              raise SlackApi::RateLimitedError.new(retry_after: 5.minutes.to_i)
            end

            true
          end
        end
      end
      now = Time.zone.parse("2026-08-23 12:00:00")
      retry_at = now + 5.minutes

      SlackDm::SyncCredential.stub(:new, sync_factory) do
        assert_enqueued_with(
          job: SlackDm::SyncCredentialJob,
          args: [ "slack-dms", retry_at ],
          at: retry_at
        ) do
          travel_to(now) { SlackDm::SyncCredentialJob.perform_now("slack-dms") }
        end

        cursor = SlackDmSyncCursor.find_by!(oauth_app_slug: "slack-dms")
        assert_equal first.id, cursor.next_credential_id
        assert_equal "D200", cursor.next_conversation_id
        assert_equal retry_at, cursor.not_before
        assert_equal [ first.id ], attempted_ids

        travel_to(now + 4.minutes) { SlackDm::SyncCredentialJob.perform_now("slack-dms") }
        assert_equal [ first.id ], attempted_ids

        rate_limited = false
        travel_to(retry_at) do
          perform_enqueued_jobs(only: SlackDm::SyncCredentialJob)
        end
      end

      assert_equal [ first.id, first.id, second.id ], attempted_ids
      cursor = SlackDmSyncCursor.find_by!(oauth_app_slug: "slack-dms")
      assert_equal first.id, cursor.next_credential_id
      assert_nil cursor.next_conversation_id
      assert_nil cursor.not_before
    end

    test "SyncCredentialJob ignores a delayed retry after another run clears the pause" do
      app = slack_app
      slack_credential(app: app)
      attempted_ids = []
      retry_at = Time.zone.parse("2026-08-23 12:20:00")
      SlackDmSyncCursor.create!(oauth_app_slug: "slack-dms")
      sync_factory = lambda do |credential|
        attempted_ids << credential.id
        Object.new
      end

      SlackDm::SyncCredential.stub(:new, sync_factory) do
        SlackDm::SyncCredentialJob.set(wait_until: retry_at).perform_later(
          "slack-dms",
          retry_at
        )
        travel_to(retry_at) do
          perform_enqueued_jobs(only: SlackDm::SyncCredentialJob)
        end
      end

      assert_empty attempted_ids
    end

    test "SyncCredentialJob stops between credentials after its runtime budget" do
      app = slack_app
      first = slack_credential(app: app)
      second = slack_credential(app: app)
      attempted_ids = []
      advance_clock = -> { travel SlackDm::SyncCredentialJob::RUN_TIME_BUDGET + 1.second }
      sync_factory = lambda do |credential|
        Object.new.tap do |sync|
          sync.define_singleton_method(:call) do |**|
            attempted_ids << credential.id
            advance_clock.call
            true
          end
        end
      end

      SlackDm::SyncCredential.stub(:new, sync_factory) do
        travel_to(Time.zone.parse("2026-08-23 12:00:00")) do
          SlackDm::SyncCredentialJob.perform_now("slack-dms")
        end
      end

      assert_equal [ first.id ], attempted_ids
      assert_equal second.id,
                   SlackDmSyncCursor.find_by!(oauth_app_slug: "slack-dms").next_credential_id
    end

    test "SyncCredentialJob preserves a conversation cursor when the budget expires" do
      app = slack_app
      first = slack_credential(app: app)
      slack_credential(app: app)
      attempted_ids = []
      sync_factory = lambda do |credential|
        Object.new.tap do |sync|
          sync.define_singleton_method(:call) do |**_kwargs, &checkpoint|
            attempted_ids << credential.id
            checkpoint.call("D200")
            false
          end
        end
      end

      SlackDm::SyncCredential.stub(:new, sync_factory) do
        SlackDm::SyncCredentialJob.perform_now("slack-dms")
      end

      cursor = SlackDmSyncCursor.find_by!(oauth_app_slug: "slack-dms")
      assert_equal [ first.id ], attempted_ids
      assert_equal first.id, cursor.next_credential_id
      assert_equal "D200", cursor.next_conversation_id
      assert_nil cursor.not_before
    end
  end
end
