# Frequently Asked Questions

This FAQ covers common questions about contributing to Labrador, working with agents in this repository, and how the project is organized. For the full contribution flow, see [CONTRIBUTING.md](CONTRIBUTING.md). For engineering details, build setup, code style, and testing, see [LABRADOR.md](LABRADOR.md).

## Contributing

### How do I contribute?

Start with a GitHub issue in this repository. Bug reports are ready to fix once triaged; feature requests should go through a short spec PR before implementation when the behavior or architecture is non-trivial. The basic flow is documented in [CONTRIBUTING.md](CONTRIBUTING.md).

### How do I file a good bug report or feature request?

For bugs, include reproduction steps, expected behavior, actual behavior, your Labrador version (`Settings -> About`), and OS. For features, describe the user-facing problem before proposing an implementation.

### How do I build and run Labrador from source?

```bash
./script/bootstrap   # platform-specific setup
./script/run         # build and run Labrador
./script/presubmit   # fmt, clippy, and tests
```

macOS, Linux, and Windows are supported. Platform-specific setup is handled by `./script/bootstrap`. See [LABRADOR.md](LABRADOR.md) for the full engineering guide.

### Will my PR be reviewed by a human or by an agent?

Use the review process requested by the maintainers for the branch or PR. Agent-generated changes should meet the same bar as hand-written changes: focused diffs, clear validation, and tests for non-trivial behavior.

## Using an Agent on This Repo

### Can I use my own coding agent to contribute?

Yes. Use whatever you like: Labrador's local agent surfaces, Claude Code, Codex, Gemini CLI, Cursor, another agent, or no agent at all. The repo ships agent-readable context in [`.agents/skills/`](.agents/skills/), [`specs/`](specs/), and [LABRADOR.md](LABRADOR.md).

### Are agent-generated PRs held to the same bar as human ones?

Yes. The same tests, formatting, linting, and review expectations apply regardless of who or what wrote the code.

### Will my issues, comments, or code be used to train models?

No project-specific model-training rights are granted by contributing to this repository. Contributions remain governed by the repository licenses and contribution process.

## What's Open Source and What Isn't

### Is Labrador fully open source?

The Labrador client code in this repository is open source under the repository licenses. Some cloud-backed services, hosted authentication, and upstream integrations may remain outside this repository.

### What lives in this repo?

The desktop client app, terminal emulator, local agent surfaces, UI framework crates, integration tests, agent skills, feature specs, and supporting Rust crates used by the application.

### Can I run Labrador without signing in or using cloud services?

Some functionality works fully locally. Other features that depend on hosted authentication, sync, teams, or remote services require backend access.

## Licensing

### Why does the repository use AGPL and MIT?

The app code is licensed under [AGPL v3](LICENSE-AGPL), while the UI framework crates are licensed under [MIT](LICENSE-MIT). See [README.md](README.md) for the short license summary.

### Can someone fork Labrador?

Yes. Forks and open derivatives are allowed under the repository licenses.

## Help and Security

### Where do I get help?

Use GitHub issues or repository discussions where available. Mention maintainers on an issue or PR when something needs triage or review.

### How do I report a security vulnerability?

Please do not open a public GitHub issue. See [SECURITY.md](SECURITY.md) for responsible disclosure guidance.
