---
name: Code Review
description: Reviews a diff for bugs
when_to_use: when a pull request is open
allowed-tools:
  - Read
  - Grep
  - Bash(git diff:*)
model: opus
argument-hint: "[pr]"
hooks:
  PreToolUse: []
---

Review the diff from !`git diff HEAD`.
