# Run using bin/ci (from services/console). Mirrors .github/workflows/console-ci.yml.

CI.run do
  step "Setup", "bin/setup --skip-server"
  step "Setup: SPA dependencies", "pnpm install --frozen-lockfile --filter @centaur/console-web"

  step "Style: Ruby", "bin/rubocop"

  step "Security: Gem audit", "bin/bundler-audit"
  step "Security: Brakeman code analysis", "bin/brakeman --quiet --no-pager --exit-on-warn --exit-on-error"

  step "Web: Typecheck", "pnpm -C web typecheck"
  step "Web: Lint", "pnpm -C web lint"
  step "Web: Test", "pnpm -C web test"
  step "Web: Build", "pnpm -C web build"

  step "Tests: Rails", "bin/rails test"
  step "Tests: Seeds", "env RAILS_ENV=test bin/rails db:seed:replant"

  # Optional: set a green GitHub commit status to unblock PR merge.
  # Requires the `gh` CLI and `gh extension install basecamp/gh-signoff`.
  # if success?
  #   step "Signoff: All systems go. Ready for merge and deploy.", "gh signoff"
  # else
  #   failure "Signoff: CI failed. Do not merge or deploy.", "Fix the issues and try again."
  # end
end
