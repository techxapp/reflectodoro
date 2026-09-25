#!/usr/bin/env node
// Bump the app version everywhere it lives, commit, tag vX.Y.Z and push.
// Pushing the tag is what triggers .github/workflows/release.yml.
//
//   npm run release -- 0.15.3            bump, commit, tag, push
//   npm run release -- 0.15.3 --dry-run  show what would change, touch nothing
//   npm run release -- 0.15.3 --no-push  bump, commit and tag locally only

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const flags = new Set(args.filter((a) => a.startsWith("--")));
const version = args.find((a) => !a.startsWith("--"))?.replace(/^v/, "");
const dryRun = flags.has("--dry-run");
const push = !flags.has("--no-push");

const fail = (msg) => {
  console.error(`error: ${msg}`);
  process.exit(1);
};
const git = (...a) =>
  execFileSync("git", a, { cwd: root, encoding: "utf8" }).trim();

if (!version || !/^\d+\.\d+\.\d+$/.test(version)) {
  fail("usage: npm run release -- <X.Y.Z> [--dry-run] [--no-push]");
}
const tag = `v${version}`;

const read = (p) => readFileSync(resolve(root, p), "utf8");
const current = JSON.parse(read("package.json")).version;
const cmp = (a, b) => {
  const [x, y] = [a, b].map((v) => v.split(".").map(Number));
  return x[0] - y[0] || x[1] - y[1] || x[2] - y[2];
};
if (cmp(version, current) <= 0) {
  fail(`${version} is not newer than the current version ${current}`);
}

// Every place the version is written. Each edit must change exactly `count`
// spots, so a file that drifts from the expected layout fails loudly instead
// of being half-bumped.
const escaped = current.replace(/\./g, "\\.");
const edits = [
  {
    file: "package.json",
    count: 1,
    apply: (s) => s.replace(new RegExp(`("version": ")${escaped}(")`), `$1${version}$2`),
  },
  {
    // Root "version" and packages[""].version.
    file: "package-lock.json",
    count: 2,
    apply: (s) =>
      s.replace(
        new RegExp(`("name": "reflectodoro",\\s+"version": ")${escaped}(")`, "g"),
        `$1${version}$2`,
      ),
  },
  {
    file: "src-tauri/Cargo.toml",
    count: 1,
    apply: (s) => s.replace(new RegExp(`^(version = ")${escaped}(")`, "m"), `$1${version}$2`),
  },
  {
    // Only the app's own [[package]] block, never a dependency that happens
    // to share the version number.
    file: "src-tauri/Cargo.lock",
    count: 1,
    apply: (s) =>
      s.replace(
        new RegExp(`(name = "reflectodoro"\\r?\\nversion = ")${escaped}(")`),
        `$1${version}$2`,
      ),
  },
  {
    file: "src-tauri/tauri.conf.json",
    count: 1,
    apply: (s) => s.replace(new RegExp(`("version": ")${escaped}(")`), `$1${version}$2`),
  },
  {
    // App target and the Live Activity extension, two keys each.
    file: "src-tauri/gen/apple/project.yml",
    count: 4,
    apply: (s) =>
      s.replace(
        new RegExp(`(CFBundle(?:ShortVersionString|Version): "?)${escaped}("?)$`, "gm"),
        `$1${version}$2`,
      ),
  },
];

// Preflight: fail before touching anything. A dry run only warns, so the
// file edits can still be checked with uncommitted work around.
const problem = dryRun ? (m) => console.warn(`warning: ${m}`) : fail;
if (git("status", "--porcelain")) {
  problem("working tree is not clean; commit or stash your changes first");
}
const branch = git("rev-parse", "--abbrev-ref", "HEAD");
if (branch !== "main") problem(`on branch '${branch}', releases are cut from main`);
if (git("tag", "--list", tag)) problem(`tag ${tag} already exists`);

const planned = edits.map((e) => {
  const before = read(e.file);
  const after = e.apply(before);
  const changed = (after.match(new RegExp(version.replace(/\./g, "\\."), "g")) || []).length -
    (before.match(new RegExp(version.replace(/\./g, "\\."), "g")) || []).length;
  if (changed !== e.count) {
    fail(`${e.file}: expected ${e.count} version replacement(s), made ${changed}`);
  }
  return { ...e, after };
});

console.log(`${current} -> ${version}  (tag ${tag}${push ? ", will push" : ", local only"})`);
for (const p of planned) console.log(`  ${p.file}  (${p.count})`);
if (dryRun) {
  console.log("dry run: nothing written");
  process.exit(0);
}

for (const p of planned) writeFileSync(resolve(root, p.file), p.after);
git("add", ...planned.map((p) => p.file));
git("commit", "-m", `Bump version to ${version}`);
git("tag", tag);
console.log(`committed and tagged ${tag}`);

if (push) {
  git("push", "origin", "HEAD");
  git("push", "origin", tag);
  console.log(`pushed main and ${tag}; the release workflow should start now`);
} else {
  console.log(`not pushed. Run: git push origin HEAD && git push origin ${tag}`);
}
