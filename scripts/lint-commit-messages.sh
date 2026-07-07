#!/usr/bin/env bash
set -e

file="$1"
if [ -z "$file" ]; then
  echo "Usage: $0 <commit-message-file>"
  echo ""
  echo "Validates a git commit message against the project conventions:"
  echo "  - Conventional commits format (type(scope)!: description)"
  echo "  - Subject: 50 characters max"
  echo "  - Body: prose paragraphs, wrapped at 72 characters"
  echo "  - Blank line between subject and body"
  exit 1
fi

subject=$(head -1 "$file")

# Allow merge/revert/squash/fixup without validation
if echo "$subject" | grep -qE '^(Merge |Revert |squash! |fixup! )'; then
  exit 0
fi

# Check conventional commits format
if ! echo "$subject" | grep -qE '^(feat|fix|chore|docs|style|refactor|perf|test|build|ci|revert)(\(.+\))!?: .+$'; then
  echo "Error: subject must match conventional commits format:"
  echo "  type(scope)!: description"
  echo "  Valid types: feat|fix|chore|docs|style|refactor|perf|test|build|ci|revert"
  echo "  Scope is required (e.g., feat(auth): description)"
  exit 1
fi

# Check subject length
subject_len=$(printf '%s' "$subject" | wc -c | tr -d ' ')
if [ "$subject_len" -gt 50 ]; then
  echo "Error: subject must be 50 characters or less (currently $subject_len)"
  exit 1
fi

# Check blank line between subject and body
second_line=$(sed -n '2p' "$file")
if [ -n "$second_line" ]; then
  echo "Error: subject and body must be separated by a blank line"
  exit 1
fi

# Check body presence
total_lines=$(wc -l < "$file" | tr -d ' ')
if [ "$total_lines" -lt 3 ]; then
  echo "Error: commit must have a body (blank line + summary of what and why)"
  exit 1
fi

# Separate body from footers (second blank line separates body from footers)
# Find line numbers of all blank lines
blank_lines=$(awk 'NF==0 {print NR}' "$file")
body_end=$total_lines
for ln in $blank_lines; do
  if [ "$ln" -gt 2 ]; then
    body_end=$((ln - 1))
    break
  fi
done

# Check body line wrapping (body only, not footers)
long_lines=$(sed -n "3,${body_end}p" "$file" | grep -n '.\{73,\}') || true
if [ -n "$long_lines" ]; then
  echo "Error: body lines must wrap at 72 characters:"
  echo "$long_lines"
  exit 1
fi

# Check body uses prose, not bullet points (body only, not footers)
bullet_lines=$(sed -n "3,${body_end}p" "$file" | grep -n '^ *[-*] ') || true
if [ -n "$bullet_lines" ]; then
  echo "Error: body must use prose paragraphs, not bullet points:"
  echo "$bullet_lines"
  exit 1
fi
