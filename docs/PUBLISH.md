# How to publish a new release

Even when a new version might affect multiple crates in this repository,
each should release should be handled individually.

A collaborator follows these steps to publish:

1. Create a new pull request with:
  1. The version in the `Cargo.toml` updated.
  2. The `CHANGELOG.md` contains what notable changes from the last version.
  3. Name the commit message as simply "$crate vX.Y.Z".
2. Another collaborator approves the pull request, but does not merge.
3. The original collaborator merges (rebase) the pull request.
4. Update locally to the merged branch.
5. Run `cargo publish -p h3` (or other crate name).
6. Tag the release: `git tag h3-vX.Y.Z`, with the appropriate crate and
   version.
7. Push the tag to GitHub: `git push upstream h3-v.X.Y.Z`.
8. Go to the GitHub Releases page, and draft a new release based on the tag.
   Include the contents from the changelog.

And celebrate, you're done!
