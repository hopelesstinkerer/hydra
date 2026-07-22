# hydra — AGENTS.md

## Project

Recursive system that captures experience, externalizes memory,
evaluates outcomes, and writes back into itself after every
execution. The user sets a goal; hydra handles the rest.

## Git workflow

- **Git Flow**: `main` (production) ← `develop` (integration) ← feature branches.
- **Branch naming**: feature branches use `feature/<name>`, fix branches use
  `fix/<name>`. No other branch prefixes.
- **Conventional commits**: `type(scope)!: description` — enforced by lefthook
  locally and GitHub Actions on push. Valid types: `feat`, `fix`, `chore`,
  `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `revert`. Scope is
  required (e.g., `feat(auth):`).
- **Subject**: 50 characters or less
- **Body**: Prose paragraphs (no bullet points), wrapped at 72 characters
- **Structure**: blank line between subject and body
- **Atomic commits**: Each commit is a single logical change. A commit
  introducing a feature includes its implementation, documentation, and tests
  together — never split across commits. A refactor commit does not also fix a
  bug. Self-containment is enforced through review, not automation.
- **Merge commits**: Merge feature branches into `develop` with `--no-ff`
  (preserve branch topology). The merge commit message MUST be a conventional
  commit following the same rules as regular commits (`type(scope): description`,
  ≤ 50 char subject). The body lists the individual commits from the feature
  branch as one-liners.
  GitHub's auto-generated `"Merge pull request #N from ..."` is NOT acceptable
  — edit it manually in the merge UI.
   ```
   feat(core): description

   <run `git log --reverse --oneline develop..HEAD` and paste the output here>
   ```

## Work style

- **Approval gating**: You have local autonomy to read, explore,
  and draft. Propose commits for approval before pushing. Do not
  push to remote branches without review.
- **Adversarial engineering**: Review each commit diff for edge
  cases, silent failures, and security gaps. Push back on unclear
  requirements with alternatives. Do not spiral into infinite self-
  doubt — the goal is progress, not perfection.
- **No forward references**: Do not mention or plan for things that do not
  exist yet. Each commit must be self-contained.
- **Single source of truth**: Define terms once in the glossary. Reference
  it from other documents rather than duplicating definitions.
- **Precision over grandeur**: Choose terms carefully. Prefer specific,
  accurate language over grandiose or inflated terms.
- **Surface assumptions**: Before implementing, state your assumptions
  explicitly. If multiple interpretations exist, present them. Do not
  hide confusion or silently pick an interpretation.
- **Simplicity first**: Minimum code that solves the problem. No
  features beyond what was asked, no abstractions for single-use code,
  no error handling for impossible scenarios. If it could be simpler,
  rewrite it.
- **Surgical changes**: Touch only what the task requires. Do not
  improve adjacent code, refactor unrelated things, or reformat
  existing style to match your preferences. Clean up only what your
  own changes made unused.
- **Iterative refinement**: Start minimal. Expand only when implementation
  requires it. Do not over-engineer for future needs.
- **Future thinking**: Document aspirational targets separately from current
  design. Keep forward-looking ideas local to avoid polluting the commit
  history with promises that cannot be kept yet.
