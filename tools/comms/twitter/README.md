# Twitter

Uses X API v2 with `X_API_KEY`. When X returns HTTP 402 (credits depleted),
public reads automatically retry through `https://api.fxtwitter.com/2`, using
the same provider endpoints as [nanocodex](https://github.com/gakonst/nanocodex/blob/master/js/x-api/src/fxtwitter.ts).
No X credentials are sent to FxTwitter.

Fallback supports posts (single and batch), profiles by handle, username batches,
latest/top search, authored timelines, followers, and following. Results preserve
Centaur's normalized fields and add `provider: "fxtwitter"`; quoted posts, articles,
media, and polls are retained when supplied. Availability and completeness depend
on the public provider. Pagination is bounded to ten pages of at most twenty items.
Provider cursors in fallback metadata belong to FxTwitter, not X.

Full-archive search, user-ID lookup, mentions, lists, liking/reposting users, quote
search, and usage/health have no equivalent fallback and retain explicit errors.
Authentication, authorization, rate-limit, network, and server errors do not trigger
fallback. FxTwitter errors (including its search-outage 404) are raised, never
converted to empty successes. Credit exhaustion is remembered until the client
closes; a new CLI invocation tries X again. A failed paginated read restarts on
FxTwitter to avoid mixing provider results and cursors.
