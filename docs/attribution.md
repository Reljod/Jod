# Attribution

Commits and PRs are Reljod's, with no Claude branding. `.claude/settings.json`
is committed (only `.claude/settings.local.json` stays local) so the policy
travels with the repo rather than living in one machine's config.

Two mechanisms:

- **No trailers.** Empty `attribution.commit` / `attribution.pr` and
  `sessionUrl: false` — no `Co-Authored-By` or `Claude-Session` line is appended
  to commits or PRs.
- **Reljod as author.** A `SessionStart` hook runs
  `git config user.name Reljod && git config user.email oretareljod@gmail.com`
  at the start of every session, overriding the agent runtime's default
  `Claude <noreply@anthropic.com>` identity. GitHub keys the commit avatar and
  name off that email, so agent-made commits show as Reljod, not `claude`.

## The Verified badge

Agent sessions have no signing key, so commits they author show as Unverified.
To get Verified under Reljod's name, sign locally with a GPG/SSH key registered
to his GitHub account (`commit.gpgsign true`). That key never enters the agent
environment.
