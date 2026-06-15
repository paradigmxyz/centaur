# iron-control dev tasks. Run `just` to list recipes.

set shell := ["bash", "-uc"]

# Hardcoded dev-only bootstrap secrets. iron-control refuses API requests until
# a user and API key exist; these are created on first boot from these vars.
# The API key must be `iak_` + 64 lowercase hex chars. NOT for production use.
export IRON_CONTROL_INITIAL_USER_EMAIL := "dev@iron.local"
export IRON_CONTROL_INITIAL_USER_PASSWORD := "dev-password-1234"
export IRON_CONTROL_INITIAL_API_KEY := "iak_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

# List available recipes.
default:
    @just --list

# Prepare the database, then run the web server + Tailwind CSS watcher under
# overmind (see Procfile.dev), so console styles rebuild on change.
dev: deps
    @echo "== Preparing database =="
    bin/rails db:prepare
    @echo "== Starting overmind (web + css watch) on http://localhost:3000 =="
    @echo "   console:  http://localhost:3000/console/principals"
    @echo "   user:     $IRON_CONTROL_INITIAL_USER_EMAIL"
    @echo "   password: $IRON_CONTROL_INITIAL_USER_PASSWORD"
    @echo "   api key:  $IRON_CONTROL_INITIAL_API_KEY"
    bin/dev

# Build the Tailwind CSS bundle once (app/assets/builds/tailwind.css).
css:
    bin/rails tailwindcss:build

# Watch and rebuild the Tailwind CSS bundle on change.
css-watch:
    bin/rails tailwindcss:watch

# Install gem dependencies if needed.
deps:
    @bundle check || bundle install

# Print the dev API key for use as a bearer token.
api-key:
    @echo "$IRON_CONTROL_INITIAL_API_KEY"

# Drop, recreate, and migrate the dev database. Re-bootstraps on next `just dev`.
db-reset:
    bin/rails db:drop db:create db:migrate

# --- Control-plane seeding ---
# Positional args map to the underlying iron:* rake tasks. References
# (principal/secret/proxy) accept an oid, foreign_id, or name. For options not
# exposed here (e.g. QUERY_PARAM), call the rake task directly: see iron.rake.

# Create a principal:  just principal-add ci-runner [foreign_id] [namespace] [k=v,..]
principal-add name foreign_id="" namespace="default" labels="":
    bin/rails iron:principal:add NAME="{{name}}" FOREIGN_ID="{{foreign_id}}" NAMESPACE="{{namespace}}" LABELS="{{labels}}"

# Create a control-plane static secret:  just secret-add acme-key sk_test_123 [header] [formatter] [foreign_id]
secret-add name value header="Authorization" formatter="" foreign_id="" namespace="default":
    bin/rails iron:secret:add NAME="{{name}}" VALUE="{{value}}" HEADER="{{header}}" FORMATTER="{{formatter}}" FOREIGN_ID="{{foreign_id}}" NAMESPACE="{{namespace}}"

# Grant a static secret to a principal:  just grant-add ci-runner acme-key
grant-add principal secret:
    bin/rails iron:grant:add PRINCIPAL="{{principal}}" SECRET="{{secret}}"

# Create a proxy, optionally assigned:  just proxy-add edge-1 [principal]
proxy-add name principal="":
    bin/rails iron:proxy:add NAME="{{name}}" PRINCIPAL="{{principal}}"

# Assign or swap a proxy's principal:  just proxy-assign edge-1 ci-runner
proxy-assign proxy principal:
    bin/rails iron:proxy:assign PROXY="{{proxy}}" PRINCIPAL="{{principal}}"

# Unassign a proxy's principal:  just proxy-unassign edge-1
proxy-unassign proxy:
    bin/rails iron:proxy:unassign PROXY="{{proxy}}"

# List control-plane resources.
principals:
    @bin/rails iron:principal:list

secrets:
    @bin/rails iron:secret:list

grants:
    @bin/rails iron:grant:list

proxies:
    @bin/rails iron:proxy:list
