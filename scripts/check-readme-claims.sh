#!/usr/bin/env bash
set -euo pipefail

if ! grep -q '^> Status: pre-release implementation' README.md; then
  echo 'README must accurately identify the currently verified implementation level.' >&2
  exit 1
fi

if grep -q '^> Status:.*first usable' README.md; then
  echo 'README must not claim a first usable release before acceptance evidence exists.' >&2
  exit 1
fi
