#!/usr/bin/env bash
set -euo pipefail

git ls-files --error-unmatch -- src >/dev/null
if git grep -n 'LruCache' -- src; then
  echo 'Reassess the lru advisory exception before using LruCache.'
  exit 1
else
  test "$?" -eq 1
fi
