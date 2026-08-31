const printHelp = () => {
  console.log(
    [
      "Usage: bump-version <major|breaking|minor|patch>",
      "",
      "Arguments:",
      "  major     Increment major version (X+1.0.0)",
      "  breaking  Alias of major",
      "  minor     Increment minor version (X.Y+1.0)",
      "  patch     Increment patch version (X.Y.Z+1)",
    ].join("\n"),
  );
};

type BumpKind = "major" | "minor" | "patch";

const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;

const parseVersion = (version: string) => {
  const match = version.match(versionPattern);

  if (!match) {
    throw new Error(`Expected a semantic version, got ${JSON.stringify(version)}.`);
  }

  return match.slice(1).map(Number) as [number, number, number];
};

const bumpVersion = (version: string, kind: BumpKind) => {
  const [major, minor, patch] = parseVersion(version);

  switch (kind) {
    case "major":
      return `${major + 1}.0.0`;
    case "minor":
      return `${major}.${minor + 1}.0`;
    case "patch":
      return `${major}.${minor}.${patch + 1}`;
  }
};

const readVersion = (content: string, file: string, section?: string) => {
  const sectionPattern = section
    ? new RegExp(`^\\[${section.replace(/[.*+?^${}()|[\\]\\\\]/g, "\\$&")}\\]\\s*$`)
    : undefined;
  let inSection = !sectionPattern;

  for (const line of content.split("\n")) {
    if (sectionPattern && line.startsWith("[")) {
      inSection = sectionPattern.test(line);
      continue;
    }

    if (inSection) {
      const match = line.match(/^version\s*=\s*"([^"]+)"\s*$/);
      if (match) return match[1];
    }
  }

  throw new Error(`Could not find a version in ${file}.`);
};

const replaceVersion = (content: string, version: string, file: string, section?: string) => {
  const oldVersion = readVersion(content, file, section);
  const expectedLine = `version = "${oldVersion}"`;
  const replacementLine = `version = "${version}"`;

  if (section) {
    const sectionStart = content.indexOf(`[${section}]`);
    const sectionEnd = content.indexOf("\n[", sectionStart + 1);
    const before = content.slice(0, sectionStart);
    const target = content.slice(sectionStart, sectionEnd === -1 ? undefined : sectionEnd);
    const after = sectionEnd === -1 ? "" : content.slice(sectionEnd);

    if (!target.includes(expectedLine)) {
      throw new Error(`Could not update the version in ${file}.`);
    }

    return before + target.replace(expectedLine, replacementLine) + after;
  }

  if (!content.includes(expectedLine)) {
    throw new Error(`Could not update the version in ${file}.`);
  }

  return content.replace(expectedLine, replacementLine);
};

const main = async () => {
  const argument = process.argv[2];

  if (argument === "--help" || argument === "-h") {
    printHelp();
    return;
  }

  const kind = argument === "breaking" ? "major" : argument;
  if (kind !== "major" && kind !== "minor" && kind !== "patch") {
    printHelp();
    process.exitCode = 1;
    return;
  }

  const cargoPath = "Cargo.toml";
  const packagerPath = "packager.toml";
  const cargo = await Bun.file(cargoPath).text();
  const packager = await Bun.file(packagerPath).text();
  const cargoVersion = readVersion(cargo, cargoPath, "workspace.package");
  const packagerVersion = readVersion(packager, packagerPath);

  if (cargoVersion !== packagerVersion) {
    throw new Error(
      `Version mismatch: ${cargoPath} is ${cargoVersion}, but ${packagerPath} is ${packagerVersion}.`,
    );
  }

  const nextVersion = bumpVersion(cargoVersion, kind);
  await Bun.write(cargoPath, replaceVersion(cargo, nextVersion, cargoPath, "workspace.package"));
  await Bun.write(packagerPath, replaceVersion(packager, nextVersion, packagerPath));
  console.log(`Bumped version: ${cargoVersion} -> ${nextVersion}`);
};

main().catch((error: unknown) => {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
});
