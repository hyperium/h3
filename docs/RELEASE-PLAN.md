# Release Plan — h3 v0.1.0

Date: 2026-11-26
Branch: `feat/release-0.0.9`
Repo: Franck-Sorel/h3 (origin) → hyperium/h3 (upstream)
Scope: Prepare a release PR only. **No `cargo publish`.**

This plan was written before any code/config edits.

## Goal

Publish a new release of the `h3` crates covering the merged changes since the
last release (2025-05-06, `h3-v0.0.8`), most notably the standard CONNECT fix
(#322) requested in GitHub Issue #369. PR #364 (header validation) is still open
and unmerged, so it is **excluded** from the changelog.

---

## 1. Target versions per crate

| Crate | Current | New | Type of bump |
|-------|---------|-----|--------------|
| `h3` | 0.0.8 | **0.1.0** | breaking (#336 adds a variant to non-`#[non_exhaustive]` `ConnectionErrorIncoming`) |
| `h3-quinn` | 0.0.10 | **0.0.11** | dependency bump + patch |
| `h3-datagram` | 0.0.2 | **0.0.3** | dependency bump + patch |
| `h3-webtransport` | 0.1.2 | **0.2.0** | breaking (accept_bi API change) |

`examples/` uses `path =` deps only and stays at 0.0.0 (not published).

## 2. Semver rationale (verified against merged code)

### `h3` → 0.1.0 (BREAKING — verified)
**#336 adds a `ConnectionClosed` variant to `quic::ConnectionErrorIncoming`**
(`h3/src/quic.rs`). That enum is a plain `#[derive(Clone)] pub enum` with **no
`#[non_exhaustive]`**, so any downstream QUIC backend that exhaustively matches it
(common for trait implementers) breaks when the variant is added. Per the project's
0.x.y convention (bump `x` on breaking), `h3` must go to **0.1.0**, not 0.0.9.

- **#336:** `ConnectionErrorIncoming::ConnectionClosed` (breaking) + new
  `QUIC_NO_ERROR: u64` const.
- **#311 (qpack module):** gated behind
  `i-implement-a-third-party-backend-and-opt-into-breaking-changes`; otherwise
  private. Additive.
- **#322 (standard CONNECT), #315 (H3_NO_ERROR), #331, #344 (backpressure),
  #360 (flush send_data), #362 (GREASE), #365 (request tracking), #323 (0-RTT),
  #318/#324 (deps):** internal fixes / additive features.

Conclusion: `h3` → **0.1.0**.

### `h3-webtransport` → 0.2.0 (BREAKING)
- **#356:** `accept_bi()` return type changed from
  `Result<Option<AcceptedBi<..>>, StreamError>` to
  `Result<Option<BiStreamResolver<..>>, ConnectionError>`, and a new public
  `BiStreamResolver` type with a `resolve()` method was introduced. The old
  direct return of `AcceptedBi` is replaced. This is a **breaking public API
  change** → bump to **0.2.0**.

### `h3-quinn` → 0.0.11 and `h3-datagram` → 0.0.3 (patch + dep bump)
- `h3-quinn`: #331 (stop_sending), #357 (recv_id while read pending), #360
  (poll_finish flush). Fixes only. Needs `h3` dependency bumped to 0.1.0 →
  patch bump to **0.0.11**.
- `h3-datagram`: #340 (preserve quarter stream id). Fix only. Depends on `h3`
  0.0.8 → bump dep and patch to **0.0.3**.

### Dependency version pins to update
- `h3-quinn/Cargo.toml`: `h3 = { version = "0.0.8" }` → `"0.1.0"`
- `h3-datagram/Cargo.toml`: `h3 = { version = "0.0.8" }` → `"0.1.0"`
- `h3-webtransport/Cargo.toml`: `h3 = { version = "0.0.8" }` → `"0.1.0"`
- `h3-quinn/Cargo.toml`: `h3-datagram = { version = "0.0.2" }` → `"0.0.3"`
- `h3-webtransport/Cargo.toml`: `h3-datagram = { version = "0.0.2" }` → `"0.0.3"`
- `examples/Cargo.toml`: path deps only → no version change needed.

Note: `Cargo.lock` is **not** tracked in this repo, so no lockfile update is needed.

## 3. Changelog bullets to add

### `changelog-h3.md` — `### v0.1.0 (2026-11-26)`
* fix: correct behavior of standard CONNECT ([#322](https://github.com/hyperium/h3/pull/322))
* fix: properly report H3_NO_ERROR when remote terminates stream ([#315](https://github.com/hyperium/h3/pull/315))
* fix: apply backpressure before reading more frames ([#344](https://github.com/hyperium/h3/pull/344))
* fix: flush data buffered by send_data before the next write and before finish ([#360](https://github.com/hyperium/h3/pull/360))
* fix: release request tracking when resolver ends early ([#365](https://github.com/hyperium/h3/pull/365))
* fix: stop_sending panic when recv stream is in-flight ([#331](https://github.com/hyperium/h3/pull/331))
* feat: expose 0-RTT detection at stream level ([#323](https://github.com/hyperium/h3/pull/323))
* feat: map QUIC ConnectionClosed to a typed error ([#336](https://github.com/hyperium/h3/pull/336))
* feat: expose `qpack` module as unstable ([#311](https://github.com/hyperium/h3/pull/311))
* Add WebSocket `:protocol` extension per RFC 9220 ([#236](https://github.com/hyperium/h3/pull/236))
* Add connect-ip (RFC9484) as a known HTTP/3 CONNECT protocol ([#273](https://github.com/hyperium/h3/pull/273))
* reduce excess GREASE ([#362](https://github.com/hyperium/h3/pull/362))
* chore: trim unused dependencies, replace `futures` with `futures-util` ([#324](https://github.com/hyperium/h3/pull/324))
* chore: update dependencies ([#318](https://github.com/hyperium/h3/pull/318))
* docs: document more duvet compliance ([#359](https://github.com/hyperium/h3/pull/359))
* docs: update documentation of recv_data/poll_recv_data ([#343](https://github.com/hyperium/h3/pull/343))

### `changelog-h3-quinn.md` — `### v0.0.11 (2026-11-26)`
* fix: stop_sending panic when recv stream is in-flight ([#331](https://github.com/hyperium/h3/pull/331))
* fix: allow checking recv_id while a read is pending ([#357](https://github.com/hyperium/h3/pull/357))
* fix: flush data buffered by send_data before finish ([#360](https://github.com/hyperium/h3/pull/360))
* feat: map QUIC ConnectionClosed to a typed error ([#336](https://github.com/hyperium/h3/pull/336))
* update h3 to 0.1.0

### `changelog-h3-datagram.md` — `### v0.0.3 (2026-11-26)`
* fix: preserve quarter stream id ([#340](https://github.com/hyperium/h3/pull/340))
* update h3 to 0.1.0

### `changelog-h3-webtransport.md` — `### v0.2.0 (2026-11-26)`
* change `accept_bi()` to return a resolving stream ([#356](https://github.com/hyperium/h3/pull/356))
* update h3 to 0.1.0

Excluded: #364 (header validation, still open).

## 4. Ordering of steps

1. ✅ Write this plan (done).
2. Add changelog sections to the 4 per-crate changelog files.
3. Bump `version` in each crate's `[package]` + update `h3` (and `h3-datagram`)
   dependency pins in the dependents.
4. Verify: `cargo build --all-features` and `cargo test --all-features` from the
   workspace root. Fix any failures introduced by the changes.
5. Commit on `feat/release-0.0.9`, push to origin, open a draft PR against
   `hyperium/h3` via `gh pr create`.

## 5. Verification / toolchain note
The worktree has `rustc`/`cargo` 1.98.1 installed. The crate `rust-version` is 1.74.
If the installed toolchain cannot build the workspace, report the exact errors
rather than skipping verification.