# Upstreams

Rules an upstream project sets for tool-assisted work in its tree. Where one is stricter than the rest of this agreement it wins; whatever it leaves open, the agreement decides.

**Which upstream.** A tree belongs to an upstream below when one of its remotes' configured URLs names it. Read the configured values with `git config --get-regexp '^remote\..*\.url$'` — `git remote -v` and `git remote get-url` print URLs after rewriting, and a rewrite can carry credentials.

**Raw material.** Where a rule keeps some text the user's own, you still do the work behind it and hand that over in its place: for a commit message, the problem, the solution, the evidence, and the trailers; for a code comment, the constraint it pins and the source that establishes it; for a review comment, the `review-protocol` finding; for an MR or PR description, the motivation, approach, impact, and open questions.

## Mesa — `gitlab.freedesktop.org/mesa/mesa`

[Submitting patches](https://docs.mesa3d.org/submittingpatches.html):

> "AI" shouldn't be used to generate code comments, commit messages, and Gitlab comments. […] contributors can use "AI" for research etc., but all text should be their own words.

- **Code** — yours to write; its commit discloses it with `Assisted-by:` or `Generated-by:`.
- **Commit messages, code comments, MR descriptions, review comments** — the user's words; hand over their raw material.
- **History** — every pushed commit builds and works on its own and no fixup commit remains, so fixups are folded before the user pushes; reviewers' `Reviewed-by:`, `Acked-by:`, and `Tested-by:` tags are amended into the commits before merge.

## LLVM — `github.com/llvm/llvm-project`

[AI tool policy](https://llvm.org/docs/AIToolPolicy.html), [GitHub workflow](https://llvm.org/docs/GitHub.html):

- **Anything you generate** — the user reviews it before anyone else sees it; substantial tool-generated content is labelled with `Assisted-by:`.
- **PR title and description** — they become the title and message of the squashed commit, and the policy strongly recommends the contributor write the description: hand over its raw material.
- **Review** — feedback is answered with fixup commits pushed on top; rebasing and force-pushing wait until they are needed or the PR is approved.
