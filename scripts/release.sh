#!/usr/bin/env bash
# Cut a release: bump the crate version, test, commit, tag.
# Pushing the v<version> tag triggers .github/workflows/release.yml, which
# builds the four targets and attaches the tarballs to the GitHub Release.
#
# Usage:
#   scripts/release.sh <version>
#   scripts/release.sh 0.2.0
#   scripts/release.sh patch    # auto-bump from current version
#   scripts/release.sh minor
#   scripts/release.sh major

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <version|patch|minor|major>" >&2
  exit 1
fi

ARG="$1"

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

if [ -n "$(git status --porcelain)" ]; then
  echo "Working tree is dirty. Commit or stash first." >&2
  git status --short >&2
  exit 1
fi

BRANCH=$(git rev-parse --abbrev-ref HEAD)
if [ "$BRANCH" != "main" ]; then
  echo "Refusing to release from branch '$BRANCH' (expected: main)." >&2
  exit 1
fi

git fetch origin --tags

CURRENT=$(awk -F'"' '/^\[package\]/ { p = 1 } p && /^version = / { print $2; exit }' Cargo.toml)

case "$ARG" in
  patch | minor | major)
    IFS=. read -r MAJ MIN PAT <<< "${CURRENT%%-*}"
    case "$ARG" in
      major) VERSION="$((MAJ + 1)).0.0" ;;
      minor) VERSION="$MAJ.$((MIN + 1)).0" ;;
      patch) VERSION="$MAJ.$MIN.$((PAT + 1))" ;;
    esac
    ;;
  *) VERSION="$ARG" ;;
esac

if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
  echo "Invalid version: $VERSION" >&2
  exit 1
fi

TAG="v$VERSION"

if git rev-parse "$TAG" > /dev/null 2>&1; then
  echo "Tag $TAG already exists." >&2
  exit 1
fi

echo "Bumping $CURRENT -> $VERSION"

# shellcheck disable=SC2064
trap "echo >&2; echo 'Release aborted with a partial bump in the tree. Undo with:' >&2; echo '  git checkout -- Cargo.toml Cargo.lock' >&2" ERR

# The [package] version only — the workflow refuses to build a tag that
# disagrees with it, and `env!("CARGO_PKG_VERSION")` is what `mcpd --version`
# reports.
awk -v v="$VERSION" '
  /^\[/ { section = $0 }
  section == "[package]" && /^version = / { sub(/"[^"]*"/, "\"" v "\"") }
  { print }
' Cargo.toml > Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml

echo "Running tests"
cargo test --all # also refreshes Cargo.lock with the new version

git add Cargo.toml Cargo.lock
git commit -m "release $TAG"
git tag -a "$TAG" -m "$TAG"
trap - ERR

echo
echo "Created commit and tag $TAG. Review with:"
echo "  git show $TAG"
echo
echo "To trigger the release workflow:"
echo "  git push origin main && git push origin $TAG"
