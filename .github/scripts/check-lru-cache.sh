#!/usr/bin/env bash
set -euo pipefail

if git grep -n 'LruCache' -- src; then
  echo 'Reassess the lru advisory exception before using LruCache.'
  exit 1
else
  test "$?" -eq 1
fi
