#!/usr/bin/env bash
#
# Tests for fork-sync.sh, the script that must never lose a local patch.
#
# It builds a synthetic upstream, mirror, and patch branch in a temp directory,
# so it is fast, offline, and does not go stale as the real upstream moves.
# Everything runs with DRY_RUN=true; nothing is pushed anywhere.
#
# Run: .github/scripts/fork-sync.test.sh

set -euo pipefail

SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/fork-sync.sh"
WORK="$(mktemp -d)"
LANDED="the feature, identical on both sides"
trap 'rm -rf "$WORK"' EXIT

pass=0
fail=0

git_q() { git -C "$WORK/clone" "$@"; }

commit() { # commit <file> <content> <message>
    printf '%s\n' "$2" >"$WORK/clone/$1"
    git_q add "$1"
    git_q commit --quiet -m "$3"
}

# Build: an "upstream" repo with some history, a bare "origin" standing in for
# GitHub whose mirror branch sits at an older upstream commit, and a patch
# branch built by the caller's function.
setup() { # setup <patch-fn>
    rm -rf "$WORK/upstream" "$WORK/origin.git" "$WORK/clone"

    git init --quiet "$WORK/upstream"
    git -C "$WORK/upstream" config user.name Test
    git -C "$WORK/upstream" config user.email test@example.invalid
    git -C "$WORK/upstream" checkout --quiet -b main

    # Base commit, then the commits the fork has not seen yet.
    printf 'base\n' >"$WORK/upstream/README.md"
    printf 'shared\n' >"$WORK/upstream/shared.txt"
    git -C "$WORK/upstream" add -A
    git -C "$WORK/upstream" commit --quiet -m "base"
    OLD="$(git -C "$WORK/upstream" rev-parse HEAD)"

    for n in 1 2 3; do
        printf 'upstream change %s\n' "$n" >>"$WORK/upstream/shared.txt"
        git -C "$WORK/upstream" add -A
        git -C "$WORK/upstream" commit --quiet -m "upstream commit ${n}"
    done

    # Upstream also grew a file with content a local patch happens to add
    # verbatim: the "your patch landed upstream" case in test 4.
    printf '%s\n' "$LANDED" >"$WORK/upstream/feature.txt"
    git -C "$WORK/upstream" add -A
    git -C "$WORK/upstream" commit --quiet -m "upstream commit 4: the feature"

    git init --quiet --bare "$WORK/origin.git"
    git -C "$WORK/upstream" push --quiet "$WORK/origin.git" "${OLD}:refs/heads/main"

    git clone --quiet "$WORK/origin.git" "$WORK/clone" 2>/dev/null
    git_q config user.name Test
    git_q config user.email test@example.invalid
    git_q remote add upstream "$WORK/upstream"
    git_q fetch --quiet upstream

    "$1"
    git_q push --quiet origin HEAD:refs/heads/spaceghost
    git_q fetch --quiet origin
}

run() {
    set +e
    ( cd "$WORK/clone" && UPSTREAM_URL="$WORK/upstream" DRY_RUN=true bash "$SCRIPT" ) \
        >"$WORK/log" 2>&1
    RC=$?
    set -e
}

check() { # check <name> <want-rc> <want-substring>
    if [[ "$RC" == "$2" ]] && grep -qF -- "$3" "$WORK/log"; then
        echo "  PASS  $1"
        pass=$((pass + 1))
    else
        echo "  FAIL  $1 (exit ${RC}, wanted ${2}; expected to find: ${3})"
        sed 's/^/        /' "$WORK/log" | tail -20
        fail=$((fail + 1))
    fi
}

echo "1. a clean patch series rebases onto new upstream commits"
setup_clean() {
    git_q checkout --quiet -B spaceghost "$OLD"
    commit local.txt "fork only" "ci(fork): add a local-only file"
}
setup setup_clean
run
check "fast-forwards the mirror" 0 "Fast-forwarding 4 commit(s)."
check "preserves the patch" 0 "1 patch commit(s) preserved"

echo "2. two patches both survive"
setup_two() {
    git_q checkout --quiet -B spaceghost "$OLD"
    commit a.txt "one" "ci(fork): patch one"
    commit b.txt "two" "ci(fork): patch two"
}
setup setup_two
run
check "preserves both patches" 0 "2 patch commit(s) preserved"

echo "3. a conflicting patch stops the run"
setup_conflict() {
    git_q checkout --quiet -B spaceghost "$OLD"
    commit shared.txt "fork edit to a file upstream also changed" \
        "ci(fork): edit a file upstream also touched"
}
setup setup_conflict
run
check "fails loudly" 1 "Sync failed: patch series conflicts with upstream"
check "names the conflicting file" 1 "shared.txt"

echo "4. a patch that landed upstream is not dropped silently"
setup_empty() {
    git_q checkout --quiet -B spaceghost "$OLD"
    # A commit of our own that adds exactly what upstream ended up adding.
    # After the rebase there is nothing left of it. Plain `git rebase` drops
    # such a commit without a word, which is the failure this script exists to
    # prevent.
    commit feature.txt "$LANDED" "ci(fork): add the feature (later landed upstream)"
}
setup setup_empty
run
check "stops on an empty patch" 1 "a local patch has become empty"

echo "5. a mirror with local commits is never force-updated"
setup_dirty_mirror() {
    git_q checkout --quiet -B mirror-dirt "$OLD"
    commit oops.txt "committed to the mirror by mistake" "oops: direct commit to main"
    git_q push --quiet --force origin HEAD:refs/heads/main
    git_q fetch --quiet origin
    git_q checkout --quiet -B spaceghost "$OLD"
    commit local.txt "fork only" "ci(fork): a local patch"
}
setup setup_dirty_mirror
run
check "refuses to force the mirror" 1 "has diverged from upstream"

echo
echo "passed ${pass}, failed ${fail}"
[[ "$fail" -eq 0 ]]
