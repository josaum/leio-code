#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BAKED_REPO_ROOT="${LEIO_CODE_BAKED_REPO_ROOT:-/workspace/baked-repo}"
DEFAULT_REPO_ROOT="${LEIO_CODE_REPO_ROOT:-/workspace/repo}"
GIT_URL="${LEIO_CODE_GIT_URL:-}"
GIT_REF="${LEIO_CODE_GIT_REF:-}"
GIT_CLONE_DEPTH="${LEIO_CODE_GIT_CLONE_DEPTH:-1}"

prepare_baked_repo() {
  export LEIO_CODE_REPO_ROOT="$BAKED_REPO_ROOT"
}

prepare_git_repo() {
  GIT_URL="$(node "$SCRIPT_DIR/validate-repo-url.mjs" "$GIT_URL")"
  export LEIO_CODE_GIT_URL="$GIT_URL"

  mkdir -p "$(dirname "$DEFAULT_REPO_ROOT")"

  if [[ ! -d "$DEFAULT_REPO_ROOT/.git" ]]; then
    rm -rf "$DEFAULT_REPO_ROOT"
    git clone --depth "$GIT_CLONE_DEPTH" "$GIT_URL" "$DEFAULT_REPO_ROOT"
  fi

  cd "$DEFAULT_REPO_ROOT"
  git remote set-url origin "$GIT_URL"
  git fetch --depth "$GIT_CLONE_DEPTH" origin

  if [[ -n "$GIT_REF" ]]; then
    git checkout --force "$GIT_REF"
    git fetch --depth "$GIT_CLONE_DEPTH" origin "$GIT_REF" || true
    git reset --hard FETCH_HEAD 2>/dev/null || true
  else
    current_branch="$(git symbolic-ref --quiet --short HEAD || true)"
    if [[ -n "$current_branch" ]]; then
      git fetch --depth "$GIT_CLONE_DEPTH" origin "$current_branch"
      git reset --hard "origin/$current_branch"
    fi
  fi

  export LEIO_CODE_REPO_ROOT="$DEFAULT_REPO_ROOT"
}

if [[ -n "$GIT_URL" ]]; then
  prepare_git_repo
else
  prepare_baked_repo
fi

exec node server.js
