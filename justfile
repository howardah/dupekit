alias bump := bump-version

[arg("kind", pattern="patch|minor|major|breaking")]
bump-version kind:
    bun scripts/bump-version.ts {{kind}}

# Build the macOS app icon from the supplied PNG, or assets/dupekit-icon.png by default.
update-icns source='assets/dupekit-icon.png':
    scripts/update-icns.sh {{source}}
