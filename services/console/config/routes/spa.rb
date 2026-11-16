# Browser navigations (GET, Accept: text/html) that no route above claimed
# serve the built index.html. API and other non-HTML requests fall through to
# the JSON 404. See SpaRequest for the excluded prefixes.
root "spa#show", constraints: SpaRequest
get "*spa_path", to: "spa#show", constraints: SpaRequest, format: false, as: :spa
