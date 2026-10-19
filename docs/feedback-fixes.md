# Fixes for feedback

How a thread in the synththing Claude project works on a fix for a `feedback` issue. The coordinator starts these threads, for `bug` issues only (docs/feedback-intake.md). A feature request (`enhancement`) gets a thread only once a developer has approved it.

## Working on a fix

The thread opens a draft pull request that links its issue ("Fixes #34"). Merges are never automatic: a person reviews and merges every PR. Test builds of the branch are started by a person, or only after a separate thread has reviewed the PR's code and nothing else.

## When the PR is closed without being merged

A closed PR always means stop. The thread unsubscribes from the PR, pushes nothing more to it, never reopens it, and doesn't start another attempt on its own.

- **The closing came with a comment** (on the PR, or a review): the thread summarizes the reason on the issue, then resolves its thread. If the comment asks for a different approach, a new attempt starts only when a developer says go.
- **No comment:** the thread leaves one note on the issue saying the fix was closed without a reason, and labels the issue `needs-decision`. Intake hands that issue on for no more work until someone removes the label or closes the issue.

Either way the issue stays open, unless whoever closed the PR closes it too.
