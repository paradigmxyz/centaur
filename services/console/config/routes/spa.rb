# Browser navigations (GET, Accept: text/html) to SPA-owned paths serve the
# built index.html; React Router takes it from there. API and other non-HTML
# requests fall through to the JSON 404. Paths still owned by a legacy HTML
# route match earlier and win. See SpaRequest for the path allowlist.
root "spa#show", constraints: SpaRequest, as: :spa_root
get "*spa_path", to: "spa#show", constraints: SpaRequest, format: false, as: :spa
