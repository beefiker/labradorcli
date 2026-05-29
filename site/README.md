# Labrador CLI Site

Static launch site for `labradorcli.com`.

## Cloudflare Pages

- Root directory: `site`
- Build command: leave blank, or use `exit 0`
- Build output directory: `/`

The download links point at `https://github.com/beefiker/labradorcli/releases/latest`.
The home page also fetches the latest public GitHub release metadata in the
browser and selects a matching asset for macOS, Windows, or Linux when assets
are available. macOS downloads prefer the DMG asset, Windows downloads prefer
the setup exe, and Linux downloads prefer AppImage or deb assets.

macOS release copy assumes the DMG was produced by the release workflow. DMGs
are signed and notarized when Apple signing secrets are configured.
