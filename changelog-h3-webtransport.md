# v0.2.0 (2026-11-26)
* **Breaking:** change `accept_bi()` to return a resolving stream — return type changes from `Result<Option<AcceptedBi<..>>, StreamError>` to `Result<Option<BiStreamResolver<..>>, ConnectionError>`; callers must now call `.resolve()` ([#356](https://github.com/hyperium/h3/pull/356))
* update h3 to 0.1.0
* update h3-datagram to 0.0.3

# v0.1.2 (2025-05-06)
* use new h3 error types
* some code cleanup

# v0.1.1
* update dependencies 
* move datagram logic to its own crate