alias bump := bump-version

[arg("kind", pattern="patch|minor|major|breaking")]
bump-version kind:
    bun scripts/bump-version.ts {{kind}}
