---
name: upstream-rebase
description: Rebase this wayvr fork onto upstream/main. Use when asked to update the fork, catch up with upstream, rebase onto upstream, or find out what changed upstream. Reports first, rebases after the user responds.
---

# Rebasing the fork onto upstream

This repo is a fork of `wayvr-org/wayvr` (remote `upstream`). Our work is a
linear stack of commits rebased onto `upstream/main` — never merged. History is
curated to read as if written today.

## Step 1 — report, then stop

`git fetch upstream`, then report concisely:

- how far ahead/behind we are, and what landed upstream (themes, not a commit dump)
- which of our commits are likely to conflict, and why
- anything upstream renamed or removed that we call — this is the real work

Distinguish shared files from fork-only ones. `flake.nix`, `nix/*.patch`,
`extras/`, `wayvr-openxr-layer*` and `wayvr-media-bridge` are fork-only and can
never conflict.

**Stop there and wait for direction.** Don't start the rebase.

## Step 2 — rebase

```bash
git branch -f backup/pre-upstream-rebase-$(date +%Y-%m-%d)
GIT_EDITOR=true git -c commit.gpgsign=false rebase --no-gpg-sign upstream/main
```

`rebase --continue` rejects `--no-gpg-sign`; use the `-c` form alone.

Rules that matter:

- **Never `git add -A` during a conflict.** Stage resolved files by name. This
  once committed an unresolved `Cargo.lock` that rode through 22 commits.
- **Never hand-resolve `Cargo.lock`.** It is `merge=binary` so git won't try:
  `git checkout --theirs Cargo.lock && nix develop --command cargo check`.
- Upstream deleted something we call → check for other callers before
  resurrecting it. Usually the right move is to drop it.

## Step 3 — verify

`nix develop --command cargo check --workspace --all-targets`

Expect breakage that merged cleanly and only fails here: renamed enum variants,
removed methods, our enum variants dropped by auto-merge, lost `use` statements.

Fold each fix into the commit that needs it via `git commit --fixup=<sha>` and
`rebase -i --autosquash` — never append a "fix build" commit. Afterwards
`git diff <backup> main` must be empty, and check every commit for markers, not
just the tip.

## Step 4 — hand over signing

Commits must be GPG-signed and you cannot sign them (no cached key, no pinentry
from a tool shell). Report the branch as unsigned and hand over:

```
git rebase --force-rebase --gpg-sign upstream/main
```

`--force-rebase` is required or git does nothing. Pushing needs a force-push;
never push unasked.

A green build says nothing about VR behaviour — say so rather than implying the
rebase is validated.
