# Labrador CLI

Labrador CLI is a Warp-based terminal application for local development
workflows. It keeps the core terminal experience close to the open-source Warp
codebase while adding Labrador-specific app, CLI, release, and website
packaging.

## Source

This repository is the public source for Labrador CLI:

<https://github.com/beefiker/labradorcli>

Labrador CLI is based on the open-source Warp repository:

<https://github.com/warpdotdev/warp>

## Building Locally

```bash
./script/bootstrap   # platform-specific setup
./script/run         # build and run Labrador
./script/presubmit   # fmt, clippy, and tests
```

See [LABRADOR.md](LABRADOR.md) for engineering notes, coding style, testing,
and platform-specific guidance.

## Website

The Cloudflare Pages site for `labradorcli.com` lives in [site](site/).

Local preview:

```bash
python3 -m http.server 4173 -d site
```

Cloudflare Pages settings:

- Root directory: `site`
- Build command: leave blank, or use `exit 0`
- Build output directory: `/`

## Releases

Release bundles are built by the `Release Bundles` GitHub Actions workflow. It
can be run manually for a tag such as `v0.1.0`, and it uploads platform app
installers to the matching GitHub Release:

- macOS: a DMG containing `Labrador.app` and an Applications drop link.
- Linux: an AppImage plus a Debian package.
- Windows: a setup installer built with Inno Setup.

macOS DMGs are Developer ID signed and notarized when signing is configured, so
Gatekeeper can verify the app after download. Configure these GitHub repository
secrets before running the release workflow:

- `LABRADOR_APPLE_TEAM_ID`
- `LABRADOR_NOTARIZATION_APPLE_ID`
- `LABRADOR_NOTARIZATION_PASSWORD`
- `LABRADOR_DEVELOPER_ID_CERT`
- `LABRADOR_DEVELOPER_ID_CERT_PASSWORD`
- `LABRADOR_CODESIGN_KEYCHAIN_PASSWORD`

If any macOS signing secret is missing, the workflow still publishes an unsigned
testing DMG.

## Licensing

Labrador CLI preserves Warp's license split:

- The UI framework crates, currently `crates/labrador_ui` and
  `crates/labrador_ui_core`, are licensed under the [MIT license](LICENSE-MIT).
- The rest of the code in this repository is licensed under
  [AGPL v3](LICENSE-AGPL).

This repository keeps the corresponding source public. If you distribute
modified builds or host network-accessible modified versions, review the AGPL
obligations for corresponding source availability.

## Open Source Dependencies

Key dependencies include:

- [Tokio](https://github.com/tokio-rs/tokio)
- [NuShell](https://github.com/nushell/nushell)
- [Fig Completion Specs](https://github.com/withfig/autocomplete)
- [Alacritty](https://github.com/alacritty/alacritty)
- [Hyper HTTP library](https://github.com/hyperium/hyper)
- [FontKit](https://github.com/servo/font-kit)
- [Core-foundation](https://github.com/servo/core-foundation-rs)
- [Smol](https://github.com/smol-rs/smol)
