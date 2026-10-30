---
name: open-pr
description: "Create a branch, commit, push, and open a GitHub pull request from the sandbox. Use when a user asks to modify a repository, commit changes, push a branch, or open or update a PR."
---

# Open a PR

Repos under `~/github/` are read-only. Never commit or push there.

## Workflow

1. Choose a short, lowercase, kebab-case slug that describes the change. Never omit it or use a numeric fallback.
2. Create a writable clone:

```bash
git-branch <org/repo> <branch-slug>
# e.g. git-branch paradigmxyz/centaur fix-auth-token-refresh
```

   The clone lives at `~/branches/<org>/<repo>` on `centaur/<branch-slug>-<timestamp>`.
3. Make the change there and run the repo's checks for the changed surfaces.
4. Commit with a Conventional Commit message, push, and open the PR with `gh pr create`. Use a Conventional Commit title.
5. Add the `Prompted by: ...` attribution line required by the system prompt to the PR body.
6. Reply with the PR link.

Push work in progress only when the user authorized remote git work. For an authorized PR, push before finishing so container recycling cannot lose the work.
