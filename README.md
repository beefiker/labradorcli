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
can be run manually for a tag such as `v0.1.0`, and it uploads a macOS DMG,
Linux tar.gz archive, and Windows zip archive to the matching GitHub Release.

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
