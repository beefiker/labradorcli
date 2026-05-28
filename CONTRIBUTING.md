# Contributing to Labrador

Thanks for helping improve Labrador. Keep changes focused, include tests for non-trivial behavior, and run the smallest relevant validation before submitting.

## Development Setup

See [README.md](README.md) and [LABRADOR.md](LABRADOR.md) for the engineering guide. Quick start:

```bash
./script/bootstrap   # platform-specific setup
cargo run            # build and run Labrador
./script/presubmit   # fmt, clippy, and tests
```

## Testing

Tests are required for most code changes:

* Bug fixes should include a regression test that would have caught the bug.
* Algorithmic or non-trivial logic needs unit tests.
* User-facing flows should have end-to-end coverage whenever the behavior can be exercised that way.

Run unit tests with `cargo nextest run`. See [LABRADOR.md](LABRADOR.md) for more detail.

## Code Style

* `cargo fmt` and `cargo clippy --workspace --all-targets --all-features --tests -- -D warnings` should pass before review.
* Prefer imports over path qualifiers, inline format args (`println!("{x}")`), and exhaustive `match` over `_` wildcards.
* Keep diffs narrow and avoid unrelated refactors.

## Code of Conduct

This project adopts the [Contributor Covenant](https://www.contributor-covenant.org/) (v2.1) as its code of conduct. See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
