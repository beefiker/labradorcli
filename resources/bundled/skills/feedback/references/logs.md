# Log And Crash Artifact Guidance

Use this only for crashes, startup failures, rendering bugs, sync issues, or hard-to-reproduce regressions.

- Ask for logs only when they are likely to improve the report.
- Note in the issue that logs or crash reports were attached, but do not claim they contain console input or output.
- In the `Artifacts` section, mention the exact file names or bundles that were attached.

macOS paths and commands:

- Logs live under `~/Library/Logs/`
- App logs are typically `~/Library/Logs/labrador.log*`
- Zip command: `zip -j ~/Desktop/labrador-logs.zip ~/Library/Logs/labrador.log*`
- If Labrador still opens, the user can search `View Labrador Logs` in the Command Palette
- Crash reports may also exist under `~/Library/Logs/DiagnosticReports/` as Labrador `.ips` files

Linux paths:

- Logs live under Labrador's state directory.
- Stable app logs are typically `~/.local/state/Labrador-Terminal/labrador.log*`
- Preview app logs are typically `~/.local/state/Labrador-Terminal-Preview/labrador.log*`
- If the exact channel is unclear, ask the user to open the nearest `labrador*.log*` files under `~/.local/state/`

Windows paths:

- Logs live under Labrador's local app data state directory.
- Stable app logs are typically `%LOCALAPPDATA%\labrador\Labrador\data\logs\labrador.log*`
- Preview app logs are typically `%LOCALAPPDATA%\labrador\LabradorPreview\data\logs\labrador.log*`
- If the exact channel is unclear, ask the user to look under `%LOCALAPPDATA%\labrador\` for the relevant `Labrador*` folder and attach the matching `labrador*.log*` files from its `data\logs\` directory

If no artifacts are available, say so plainly instead of implying they were checked.
