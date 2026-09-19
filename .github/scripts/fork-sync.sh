#!/usr/bin/env bash
#
# Keep this fork current with upstream while preserving the local patch series.
#
# Branch model (see the README's "Fork maintenance" section):
#
#   main        A pristine mirror of upstream. Fast-forward only; never edited
#               here, so `git diff main..spaceghost` is always exactly the local
#               delta and a topic branch cut from `main` is clean to send
#               upstream.
#   spaceghost  The default branch: upstream's tip plus the local patches. The
#               series is rebased onto each new upstream tip so it stays a
#               reviewable stack rather than a thicket of merge commits.
#
# The contract is that nothing is lost quietly. A conflict, a patch that has
# become empty (i.e. it landed upstream), or a lost commit stops the run,
# leaves both remote branches exactly as they were, and files an issue. Every
# force-push is preceded by a backup tag, so the pre-rebase tip is always
# recoverable.
#
# Env:
#   UPSTREAM_URL     default https://github.com/moq-dev/moq.git
#   UPSTREAM_BRANCH  default main
#   MIRROR_BRANCH    default main
#   PATCH_BRANCH     default spaceghost
#   DRY_RUN          "true" to do all the work and push nothing
#   GH_TOKEN         needed only to file an issue on failure

set -euo pipefail

UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/moq-dev/moq.git}"
UPSTREAM_BRANCH="${UPSTREAM_BRANCH:-main}"
MIRROR_BRANCH="${MIRROR_BRANCH:-main}"
PATCH_BRANCH="${PATCH_BRANCH:-spaceghost}"
DRY_RUN="${DRY_RUN:-false}"

# Scratch branch the rebase happens on, so a failure never touches a real ref.
WORK_BRANCH="_fork_sync_work"

log() { printf '\n=== %s\n' "$*"; }

# Append to the Actions run summary when there is one, so the outcome is
# readable without opening the log.
summary() {
    printf '%s\n' "$*"
    [[ -n "${GITHUB_STEP_SUMMARY:-}" ]] && printf '%s\n' "$*" >>"$GITHUB_STEP_SUMMARY"
    return 0
}

# File (or update) a single tracking issue so a broken sync is impossible to
# miss, then fail the run. Reuses one open issue instead of opening one a day.
fail_loudly() {
    local title="$1" body="$2"

    summary "**Sync failed: ${title}**"
    summary ''
    summary "$body"

    if [[ -z "${GH_TOKEN:-}" ]] || ! command -v gh >/dev/null 2>&1; then
        echo "No GH_TOKEN/gh available; not filing an issue." >&2
        exit 1
    fi

    local run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:-}/actions/runs/${GITHUB_RUN_ID:-}"
    local full="${body}"$'\n\n'"Run: ${run_url}"$'\n\n'"Both \`${MIRROR_BRANCH}\` and \`${PATCH_BRANCH}\` were left untouched. Resolve it locally:"$'\n\n'"\`\`\`sh"$'\n'"git fetch origin && git fetch upstream"$'\n'"git checkout ${PATCH_BRANCH} && git rebase --onto upstream/${UPSTREAM_BRANCH} \$(git merge-base ${PATCH_BRANCH} upstream/${UPSTREAM_BRANCH}) "$'\n'"# fix, then: git push --force-with-lease origin ${PATCH_BRANCH}"$'\n'"\`\`\`"

    local existing
    existing="$(gh issue list --state open --label fork-sync --limit 1 --json number --jq '.[0].number' 2>/dev/null || true)"

    if [[ -n "$existing" && "$existing" != "null" ]]; then
        gh issue comment "$existing" --body "$full" >/dev/null && echo "Commented on issue #${existing}." || true
    else
        # --label can fail if the label does not exist yet; create it first.
        gh label create fork-sync --description "Upstream sync automation" --color B60205 >/dev/null 2>&1 || true
        gh issue create --title "fork-sync: ${title}" --body "$full" --label fork-sync >/dev/null 2>&1 ||
            gh issue create --title "fork-sync: ${title}" --body "$full" >/dev/null || true
    fi

    exit 1
}

# ---------------------------------------------------------------- fetch

log "Fetching upstream (${UPSTREAM_URL} ${UPSTREAM_BRANCH})"
if git remote get-url upstream >/dev/null 2>&1; then
    git remote set-url upstream "$UPSTREAM_URL"
else
    git remote add upstream "$UPSTREAM_URL"
fi
git fetch --quiet upstream "$UPSTREAM_BRANCH"
git fetch --quiet origin

UPSTREAM_TIP="$(git rev-parse "upstream/${UPSTREAM_BRANCH}")"
echo "upstream tip: ${UPSTREAM_TIP}"

# ---------------------------------------------------------------- mirror

log "Mirror branch: ${MIRROR_BRANCH}"
MIRROR_TIP="$(git rev-parse "origin/${MIRROR_BRANCH}")"

if [[ "$MIRROR_TIP" == "$UPSTREAM_TIP" ]]; then
    echo "Already at the upstream tip."
    MIRROR_ACTION="already current"
elif git merge-base --is-ancestor "$MIRROR_TIP" "$UPSTREAM_TIP"; then
    COUNT="$(git rev-list --count "${MIRROR_TIP}..${UPSTREAM_TIP}")"
    echo "Fast-forwarding ${COUNT} commit(s)."
    if [[ "$DRY_RUN" == "true" ]]; then
        echo "DRY_RUN: not pushing."
    else
        # No --force: if this is not a fast-forward, the push must fail.
        git push origin "${UPSTREAM_TIP}:refs/heads/${MIRROR_BRANCH}"
    fi
    MIRROR_ACTION="fast-forwarded ${COUNT} commit(s)"
else
    # Someone committed to the mirror, or upstream rewrote history. Either way a
    # human decides; this script will not force a pristine mirror.
    fail_loudly "mirror branch \`${MIRROR_BRANCH}\` has diverged from upstream" \
        "\`${MIRROR_BRANCH}\` (\`${MIRROR_TIP}\`) is not an ancestor of \`upstream/${UPSTREAM_BRANCH}\` (\`${UPSTREAM_TIP}\`), so it cannot be fast-forwarded. The mirror is supposed to contain nothing but upstream commits. Local work belongs on \`${PATCH_BRANCH}\`.

Commits on the mirror that upstream does not have:

\`\`\`
$(git log --oneline --no-decorate "${UPSTREAM_TIP}..${MIRROR_TIP}" | head -50)
\`\`\`"
fi

# ---------------------------------------------------------------- patches

log "Patch branch: ${PATCH_BRANCH}"

if ! git rev-parse --verify --quiet "origin/${PATCH_BRANCH}" >/dev/null; then
    echo "No ${PATCH_BRANCH} branch; creating it at the upstream tip."
    if [[ "$DRY_RUN" != "true" ]]; then
        git push origin "${UPSTREAM_TIP}:refs/heads/${PATCH_BRANCH}"
    fi
    summary "- mirror \`${MIRROR_BRANCH}\`: ${MIRROR_ACTION}"
    summary "- patches \`${PATCH_BRANCH}\`: created at upstream tip"
    exit 0
fi

PATCH_TIP="$(git rev-parse "origin/${PATCH_BRANCH}")"
BASE="$(git merge-base "$PATCH_TIP" "$UPSTREAM_TIP")"
PATCH_COUNT="$(git rev-list --count --no-merges "${BASE}..${PATCH_TIP}")"

echo "patch tip:  ${PATCH_TIP}"
echo "shared base: ${BASE}"
echo "local patch commits: ${PATCH_COUNT}"
git log --oneline --no-decorate "${BASE}..${PATCH_TIP}" | sed 's/^/  /'

if [[ "$BASE" == "$UPSTREAM_TIP" ]]; then
    echo "Patch branch already sits on the upstream tip; nothing to rebase."
    summary "- mirror \`${MIRROR_BRANCH}\`: ${MIRROR_ACTION}"
    summary "- patches \`${PATCH_BRANCH}\`: already current (${PATCH_COUNT} patch commit(s))"
    exit 0
fi

log "Rebasing ${PATCH_COUNT} patch commit(s) onto ${UPSTREAM_TIP}"

git checkout --quiet -B "$WORK_BRANCH" "$PATCH_TIP"

# --empty=stop is the point of this whole script: when a local patch has landed
# upstream the rebase would otherwise drop it silently. Stopping turns "your
# change disappeared" into "a human confirms the change is now upstream".
if ! git -c core.editor=true rebase --empty=stop --onto "$UPSTREAM_TIP" "$BASE" "$WORK_BRANCH"; then
    CONFLICTS="$(git diff --name-only --diff-filter=U 2>/dev/null || true)"
    STOPPED_AT="$(git log --oneline --no-decorate -1 REBASE_HEAD 2>/dev/null || true)"
    git rebase --abort 2>/dev/null || true
    git checkout --quiet --detach "$UPSTREAM_TIP" 2>/dev/null || true

    if [[ -n "$CONFLICTS" ]]; then
        fail_loudly "patch series conflicts with upstream" \
            "Rebasing \`${PATCH_BRANCH}\` onto \`${UPSTREAM_TIP}\` hit a conflict at:

\`\`\`
${STOPPED_AT}
\`\`\`

Conflicting files:

\`\`\`
${CONFLICTS}
\`\`\`"
    else
        fail_loudly "a local patch has become empty (it probably landed upstream)" \
            "The rebase of \`${PATCH_BRANCH}\` onto \`${UPSTREAM_TIP}\` stopped because this commit is now empty against upstream:

\`\`\`
${STOPPED_AT}
\`\`\`

That usually means the change is upstream now and the local copy should be dropped. Confirm it, then drop it by hand (\`git rebase --onto upstream/${UPSTREAM_BRANCH} <base>\` and \`git rebase --skip\` at that commit). This is deliberately not automatic."
    fi
fi

# Belt and braces: the rebase reported success, so the commit count must match.
NEW_COUNT="$(git rev-list --count --no-merges "${UPSTREAM_TIP}..${WORK_BRANCH}")"
if [[ "$NEW_COUNT" != "$PATCH_COUNT" ]]; then
    fail_loudly "patch count changed during rebase (${PATCH_COUNT} -> ${NEW_COUNT})" \
        "The rebase of \`${PATCH_BRANCH}\` onto \`${UPSTREAM_TIP}\` reported success but the number of patch commits changed, so something was dropped. Nothing was pushed. Rebase by hand and inspect."
fi

NEW_TIP="$(git rev-parse "$WORK_BRANCH")"
echo "rebased tip: ${NEW_TIP} (${NEW_COUNT} patch commit(s) preserved)"

if [[ "$DRY_RUN" == "true" ]]; then
    summary "- mirror \`${MIRROR_BRANCH}\`: ${MIRROR_ACTION} (dry run, not pushed)"
    summary "- patches \`${PATCH_BRANCH}\`: ${NEW_COUNT} commit(s) rebase cleanly onto \`${UPSTREAM_TIP}\` (dry run, not pushed)"
    exit 0
fi

# A rebase force-push is the one destructive step here. Tag the old tip first so
# it is always recoverable, even though --force-with-lease already guards
# against clobbering a push that arrived since the fetch.
BACKUP_TAG="fork-sync/backup-$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short "$PATCH_TIP")"
git tag "$BACKUP_TAG" "$PATCH_TIP"
git push --quiet origin "refs/tags/${BACKUP_TAG}"
echo "backup tag: ${BACKUP_TAG}"

git push --force-with-lease="refs/heads/${PATCH_BRANCH}:${PATCH_TIP}" \
    origin "${NEW_TIP}:refs/heads/${PATCH_BRANCH}"

summary "- mirror \`${MIRROR_BRANCH}\`: ${MIRROR_ACTION}"
summary "- patches \`${PATCH_BRANCH}\`: ${NEW_COUNT} commit(s) rebased onto \`$(git rev-parse --short "$UPSTREAM_TIP")\`"
summary "- backup of the previous tip: \`${BACKUP_TAG}\`"
