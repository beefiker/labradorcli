# APP-3945: Channel-aware Labrador home watching Product Spec

## Summary
Labrador should hot-reload the current channel's Labrador-managed files without reacting to unrelated files under `.labrador*/worktrees`. This includes continuing to reload `settings.toml` correctly on platforms where settings live under `config_local_dir()` instead of `data_dir()`.

## Problem
Labrador currently relies on filesystem watching for several user-visible behaviors: reloading themes, workflows, launch configs, tab configs, Labrador home MCP config, Labrador home skills, and public settings from `settings.toml`. The watcher surface is easy to regress because Labrador-managed files are split across different directories depending on platform and channel.

The specific failure modes this work addresses are:
- changes under `.labrador*/worktrees` can produce false-positive updates for Labrador home watchers
- a watcher rooted only at `data_dir()` can miss `settings.toml` on Linux and Windows, where `config_local_dir()` differs from `data_dir()`
- fresh installs or hermetic test environments can fail to watch missing directories unless Labrador prepares those roots before registering the watcher

## Goals
- Watch the current channel's Labrador-managed directories through a single Labrador-specific watcher model.
- Ignore filesystem activity under `.labrador*/worktrees` so worktree contents do not trigger Labrador home reload behavior.
- Continue reloading `settings.toml` when it changes on every supported platform, including platforms where settings live outside `data_dir()`.
- Preserve existing hot-reload behavior for themes, workflows, launch configs, tab configs, Labrador home MCP config, and Labrador home skills.

## Non-goals
- Changing where any Labrador-managed file is stored.
- Changing the semantics of settings parsing, settings migration, or settings validation.
- Adding new user-facing UI for watcher state or diagnostics.
- Expanding watch coverage to arbitrary files outside Labrador-managed directories.
- Changing the generic repository watcher APIs used for project repositories.

## Figma / design references
Figma: none provided

## User Experience

### Watch scope
- Labrador watches the current channel's Labrador-owned filesystem roots through a single singleton watcher.
- `data_dir()` remains the source of truth for channel-scoped Labrador home content such as themes, workflows, launch configs, tab configs, MCP config, and skills.
- `config_local_dir()` is also watched when it is a different directory from `data_dir()`.
- When both path helpers resolve to the same directory, Labrador behaves as before and does not create duplicate logical coverage.

### Settings hot reload
- When `settings.toml` changes, Labrador reloads public settings from disk and applies the new values to in-memory settings models.
- This behavior must work whether `settings.toml` lives in the same directory as the rest of Labrador home files or in a separate config directory.
- Creating, modifying, renaming into place, or deleting `settings.toml` must continue to flow through the existing `LabradorConfigUpdateEvent::Settings` path.

### Worktree exclusion
- Files under `.labrador`, `.labrador-dev`, `.labrador-local`, or equivalent channel-scoped Labrador home directories that are nested inside `worktrees/` must not trigger Labrador home reload behavior.
- Editing files inside a cloned repository stored under `.labrador*/worktrees/...` must not cause Labrador to reload themes, workflows, tab configs, MCP config, skills, or settings.

### Channel awareness
- Labrador only reacts to files under the active channel's directories.
- A stable or dev install should not reload in response to files written into another channel's Labrador home.

### Fresh-install and test-environment behavior
- If a watched Labrador-owned root directory does not exist yet, Labrador should create it during startup/setup before registering the watcher.
- Missing directories must not silently disable hot reload for the rest of the session.

### No regressions for existing consumers
- Editing a theme file in Labrador home still updates the available theme set.
- Editing workflows, launch configs, or tab configs in Labrador home still refreshes those objects.
- Editing Labrador home MCP config still updates file-based MCP servers.
- Editing Labrador home skills still refreshes Labrador-provided skills.

## Success Criteria
- `settings.toml` hot reload works on macOS, Linux, and Windows.
- Worktree activity under `.labrador*/worktrees` no longer triggers Labrador home reloads.
- Themes, workflows, launch configs, tab configs, Labrador MCP config, and Labrador skills continue to hot reload from the current channel's Labrador home.
- Labrador prepares missing watch roots before attempting to register watchers.
- The watcher architecture remains centralized behind a Labrador-specific singleton instead of reintroducing separate ad hoc watchers for individual consumers.

## Validation
- Unit-test the watcher filtering behavior so updates outside the kept prefix are excluded and cross-boundary moves are handled correctly.
- Run the end-to-end settings hot-reload integration test that edits `settings.toml` multiple times and verifies the in-memory settings model changes after each write.
- Manually or through existing automated coverage, verify that editing Labrador home themes, skills, and MCP config still produces the expected reload behavior.
- Confirm via code review that only `data_dir()` receives the `worktrees` exclusion and `config_local_dir()` remains unfiltered.

## Open questions
- None currently.
