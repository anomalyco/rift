#!/usr/bin/env node

import { execFileSync, spawnSync } from "node:child_process"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"

const root = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..")
const args = new Set(process.argv.slice(2))
const libraryCrates = ["crates/core"]
const lints = new Set(["dead_code", "unused_imports", "unused_variables", "unused_mut", "unused_macros", "unreachable_pub"])
const knipProjects = ["plugins/opencode", "npm/rift-snapshot"]
const searchExcludes = ["!target", "!**/node_modules", "!*.lock", "!Cargo.lock", "!scripts/dead-code.mjs"]
const testPath = /(^|\/)(tests?|benches|test_support)\/|(^|[/_])tests\.rs:|\.test\.ts:/

function run(command, commandArgs, options = {}) {
  const result = spawnSync(command, commandArgs, { cwd: root, encoding: "utf8", maxBuffer: 1 << 28, ...options })
  if (result.error) throw result.error
  return result
}

function tracked(prefix) {
  return execFileSync("git", ["ls-files", prefix], { cwd: root, encoding: "utf8" }).split("\n").filter(Boolean)
}

function references(name, definition, { docs = false } = {}) {
  const globs = docs ? searchExcludes : [...searchExcludes, "!*.md"]
  const result = run("rg", ["--line-number", "--word-regexp", "--fixed-strings", ...globs.flatMap((glob) => ["--glob", glob]), name, "."])
  return result.stdout
    .split("\n")
    .filter(Boolean)
    .map((line) => line.replace(/^\.\//, ""))
    .filter((line) => !line.startsWith(`${definition.file}:${definition.line}:`))
}

function copyTree() {
  const tree = fs.mkdtempSync(path.join(os.tmpdir(), "rift-dead-code-"))
  const files = execFileSync("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard"], { cwd: root })
  for (const file of files.toString().split("\0").filter(Boolean)) {
    const source = path.join(root, file)
    if (!fs.existsSync(source)) continue
    fs.mkdirSync(path.dirname(path.join(tree, file)), { recursive: true })
    fs.copyFileSync(source, path.join(tree, file))
  }
  return tree
}

function rewriteLines(tree, files, transform) {
  const edits = []
  for (const file of files) {
    const target = path.join(tree, file)
    const lines = fs.readFileSync(target, "utf8").split("\n")
    lines.forEach((line, index) => {
      const change = transform(line)
      if (!change) return
      edits.push({ file, line: index + 1, original: line, ...change })
      lines[index] = change.text
    })
    fs.writeFileSync(target, lines.join("\n"))
  }
  return edits
}

function unmaskDeadCodeAllows(tree) {
  return rewriteLines(tree, tracked("crates").filter((file) => file.endsWith(".rs")), (line) => {
    if (!/allow\(dead_code\)/.test(line)) return null
    const text = line.replace(/#!?\[allow\(dead_code\)\]/, "").replace(/#\[cfg_attr\([^\]]*allow\(dead_code\)\)\]/, "")
    return { kind: "unmasked", text }
  })
}

function narrowLibraryPubToCrate(tree) {
  const files = libraryCrates.flatMap((crate) => tracked(`${crate}/src`).filter((file) => file.endsWith(".rs")))
  return rewriteLines(tree, files, (line) => {
    const match = line.match(/^\s*pub\s+(?:(?:unsafe|async|const|extern\s+"C")\s+)*(?:fn|struct|enum|trait|type|const|static)\s+([A-Za-z_][A-Za-z0-9_]*)|^\s*pub\s+([a-z_][a-z0-9_]*)\s*:/)
    if (!match) return null
    return { kind: "narrowed", name: match[1] ?? match[2], text: line.replace(/\bpub\b/, "pub(crate)") }
  })
}

function restorePubNamedInErrors(tree, edits, errors) {
  const names = new Set(errors.flatMap((error) => [...error.matchAll(/`([^`]+)`/g)].map((match) => match[1].split("::").pop())))
  const reverted = edits.filter((edit) => edit.kind === "narrowed" && !edit.reverted && names.has(edit.name))
  for (const edit of reverted) {
    const target = path.join(tree, edit.file)
    const lines = fs.readFileSync(target, "utf8").split("\n")
    lines[edit.line - 1] = edit.original
    fs.writeFileSync(target, lines.join("\n"))
    edit.reverted = true
  }
  return reverted.length
}

// The Linux pass runs in a container on a fresh copy of the tree, because a
// bind mount can serve stale file contents right after the host rewrites them.
function linuxClippy(tree, cargoArgs) {
  const copy = fs.mkdtempSync(path.join(os.tmpdir(), "rift-dead-code-linux-"))
  fs.cpSync(tree, copy, { recursive: true })
  try {
    return run("docker", [
      "run", "--rm",
      "-v", `${copy}:/src`,
      "-v", "rift-dead-code-registry:/usr/local/cargo/registry",
      "-v", "rift-dead-code-target:/target",
      "-e", "CARGO_TARGET_DIR=/target",
      "-w", "/src",
      "rust:latest",
      "sh", "-c", `rustup component add clippy >/dev/null 2>&1 && cargo ${cargoArgs.map((arg) => `'${arg}'`).join(" ")}`,
    ])
  } finally {
    fs.rmSync(copy, { recursive: true, force: true })
  }
}

function cargoMessages(tree, platform) {
  const cargoArgs = ["clippy", "--workspace", "--all-targets", "--locked", "--message-format=json", "--", "-W", "dead_code", "-W", "unreachable_pub"]
  const result =
    platform === "host"
      ? run("cargo", cargoArgs, { cwd: tree, env: { ...process.env, CARGO_TARGET_DIR: path.join(root, "target", "dead-code") } })
      : linuxClippy(tree, cargoArgs)
  const messages = result.stdout
    .split("\n")
    .filter((line) => line.startsWith("{"))
    .map((line) => JSON.parse(line))
    .filter((message) => message.reason === "compiler-message")
  const errors = messages.filter((message) => message.message.level === "error").map((message) => message.message.rendered)
  if (result.status !== 0 && errors.length === 0) throw new Error(`cargo clippy failed on ${platform}\n${result.stderr.slice(-4000)}`)
  return {
    errors,
    warnings: messages
      .filter((message) => lints.has(message.message.code?.code))
      .map((message) => ({ target: message.target.name, diagnostic: message.message })),
  }
}

function rustFindings() {
  const tree = copyTree()
  const edits = [...unmaskDeadCodeAllows(tree), ...narrowLibraryPubToCrate(tree)]
  const platforms = args.has("--linux") ? ["host", "linux"] : ["host"]
  let results
  for (;;) {
    results = platforms.map((platform) => ({ platform, ...cargoMessages(tree, platform) }))
    const errors = results.flatMap((result) => result.errors.map((error) => `[${result.platform}] ${error}`))
    if (errors.length === 0) break
    if (restorePubNamedInErrors(tree, edits, errors) === 0) throw new Error(`narrowed tree does not compile\n${errors.join("\n")}`)
  }
  const kinds = new Map(edits.filter((edit) => !edit.reverted).map((edit) => [`${edit.file}:${edit.line}`, edit.kind]))
  const findings = new Map()
  for (const { platform, warnings } of results) {
    for (const { target, diagnostic } of warnings) {
      for (const span of diagnostic.spans.filter((span) => span.is_primary)) {
        const file = span.file_name.replace(/^\/src\//, "")
        const name = span.text[0]?.text.slice(span.text[0].highlight_start - 1, span.text[0].highlight_end - 1) ?? ""
        const key = `${diagnostic.code.code}:${file}:${span.line_start}:${name}`
        const finding = findings.get(key) ?? {
          source: diagnostic.code.code === "unreachable_pub" ? "visibility" : "rust",
          file,
          line: span.line_start,
          message: `${diagnostic.code.code}: ${name}`,
          names: /^[A-Za-z_][A-Za-z0-9_]*$/.test(name) ? [name] : [],
          via: kinds.get(`${file}:${span.line_start}`) ?? "plain",
          targets: new Set(),
        }
        finding.targets.add(`${platform}/${target}`)
        findings.set(key, finding)
      }
    }
  }
  fs.rmSync(tree, { recursive: true, force: true })
  return flaggedByEveryTargetCompilingTheFile([...findings.values()])
}

function flaggedByEveryTargetCompilingTheFile(findings) {
  const compiledBy = new Map()
  for (const finding of findings) {
    const targets = compiledBy.get(finding.file) ?? new Set()
    for (const target of finding.targets) targets.add(target)
    compiledBy.set(finding.file, targets)
  }
  return findings.filter((finding) => finding.source !== "rust" || finding.targets.size === compiledBy.get(finding.file).size)
}

// rustc never flags variants of a pub enum, and enums other crates use stay pub.
function exportedEnumVariants() {
  return libraryCrates.flatMap((crate) =>
    tracked(`${crate}/src`)
      .filter((file) => file.endsWith(".rs"))
      .flatMap((file) => {
        const findings = []
        let enumName = null
        fs.readFileSync(path.join(root, file), "utf8").split("\n").forEach((line, index) => {
          const start = line.match(/^pub enum ([A-Za-z0-9_]+)/)
          if (start) enumName = start[1]
          else if (enumName && /^}/.test(line)) enumName = null
          else if (enumName) {
            const variant = line.match(/^ {4}([A-Z][A-Za-z0-9_]*)\b/)
            if (variant && references(variant[1], { file, line: index + 1 }).length === 0) {
              findings.push({ source: "rust", file, line: index + 1, message: `variant \`${enumName}::${variant[1]}\` is never named`, names: [variant[1]] })
            }
          }
        })
        return findings
      }),
  )
}

function machete() {
  const result = run("cargo", ["machete"])
  if (result.status !== 0 && result.status !== 1) throw new Error(`cargo machete failed\n${result.stderr}`)
  return result.stdout
    .split("\n")
    .filter((line) => /^\t/.test(line))
    .map((line) => ({ source: "deps", file: "Cargo.toml", line: 0, message: `unused dependency ${line.trim()}`, names: [] }))
}

function workspaceDependencies() {
  const manifest = fs.readFileSync(path.join(root, "Cargo.toml"), "utf8")
  const table = manifest.split("[workspace.dependencies]")[1]?.split(/^\[/m)[0] ?? ""
  const members = tracked("crates").filter((file) => file.endsWith("Cargo.toml")).map((file) => fs.readFileSync(path.join(root, file), "utf8"))
  return [...table.matchAll(/^([A-Za-z0-9_-]+)\s*=/gm)]
    .map((match) => match[1])
    .filter((name) => !members.some((member) => new RegExp(`^${name}(\\.workspace|\\s*=\\s*\\{[^}]*workspace\\s*=\\s*true)`, "m").test(member)))
    .map((name) => ({ source: "deps", file: "Cargo.toml", line: 0, message: `workspace dependency ${name} is not inherited by any member`, names: [], verdict: "dead" }))
}

function unusedLocals() {
  const cwd = path.join(root, "plugins/opencode")
  const result = run("bunx", ["tsc", "--noEmit", "--noUnusedLocals", "--noUnusedParameters"], { cwd })
  return result.stdout
    .split("\n")
    .map((line) => line.match(/^(.+?)\((\d+),\d+\): error TS\d+: (.+)$/))
    .filter(Boolean)
    .map(([, file, line, message]) => ({ source: "tsc", file: path.join("plugins/opencode", file), line: Number(line), message, names: [] }))
}

function knip() {
  return knipProjects.flatMap((project) => {
    const cwd = path.join(root, project)
    const install = run("bun", ["install", "--ignore-scripts", "--no-save"], { cwd })
    if (install.status !== 0) throw new Error(`bun install failed in ${project}\n${install.stderr}`)
    const result = run("bunx", ["knip@6.39.0", "--no-progress", "--reporter", "json"], { cwd })
    const json = result.stdout.indexOf("{")
    if (json < 0) throw new Error(`knip failed in ${project}\n${result.stderr}`)
    const report = JSON.parse(result.stdout.slice(json))
    return report.issues.flatMap((issue) =>
      Object.entries(issue).flatMap(([kind, entries]) =>
        Array.isArray(entries)
          ? entries.map((entry) => ({ source: "knip", file: path.join(project, issue.file), line: entry.line ?? 0, message: `${kind}: ${entry.name}`, names: [entry.name] }))
          : [],
      ),
    )
  })
}

function scriptFiles() {
  return tracked("scripts")
    .filter((file) => file !== "scripts/dead-code.mjs")
    .flatMap((file) => {
      const findings = []
      const callers = references(path.basename(file), { file, line: 0 }, { docs: true }).filter((hit) => !hit.startsWith(`${file}:`))
      if (callers.length === 0) findings.push({ source: "scripts", file, line: 1, message: `script ${file} is never invoked`, names: [path.basename(file)] })
      if (file.endsWith(".sh")) {
        fs.readFileSync(path.join(root, file), "utf8").split("\n").forEach((line, index) => {
          const match = line.match(/^([A-Za-z_][A-Za-z0-9_]*)\(\)\s*\{/)
          if (!match) return
          const calls = run("rg", ["--count-matches", "--word-regexp", match[1], file]).stdout.trim()
          if (Number(calls) <= 1) findings.push({ source: "scripts", file, line: index + 1, message: `shell function ${match[1]} is never called`, names: [match[1]] })
        })
      }
      return findings
    })
}

function ffiExports() {
  return tracked("crates/ffi/src").flatMap((file) =>
    fs.readFileSync(path.join(root, file), "utf8").split("\n").flatMap((line, index, lines) => {
      const match = line.match(/extern "C" fn ([A-Za-z0-9_]+)/)
      if (!match || !/no_mangle/.test(lines[index - 1] ?? "")) return []
      const callers = references(match[1], { file, line: index + 1 }).filter((hit) => !hit.startsWith("crates/ffi/"))
      return callers.length === 0 ? [{ source: "ffi", file, line: index + 1, message: `exported symbol ${match[1]} has no caller outside crates/ffi`, names: [match[1]] }] : []
    }),
  )
}

const findings = [...rustFindings(), ...exportedEnumVariants(), ...machete(), ...workspaceDependencies(), ...knip(), ...unusedLocals(), ...scriptFiles(), ...ffiExports()]
for (const finding of findings.filter((finding) => !finding.verdict)) {
  finding.references = [...new Set(finding.names.flatMap((name) => references(name, finding)))]
  if (finding.source === "visibility") finding.verdict = "visibility"
  else if (finding.names.length === 0) finding.verdict = "review"
  else if (finding.references.length === 0) finding.verdict = "dead"
  else if (!testPath.test(`${finding.file}:`) && finding.references.every((hit) => testPath.test(hit))) finding.verdict = "test-only"
  else finding.verdict = "review"
}

const order = { dead: 0, "test-only": 1, review: 2, visibility: 3 }
findings.sort((a, b) => order[a.verdict] - order[b.verdict] || a.file.localeCompare(b.file) || a.line - b.line)
for (const verdict of Object.keys(order)) {
  const group = findings.filter((finding) => finding.verdict === verdict)
  console.log(`## ${verdict} (${group.length})`)
  for (const finding of group) {
    const scope = finding.targets ? ` [${finding.via}; ${[...finding.targets].sort().join(",")}]` : ""
    console.log(`${finding.file}:${finding.line} ${finding.source}: ${finding.message}${scope}`)
    if (verdict === "review" || verdict === "test-only") for (const hit of finding.references.slice(0, 8)) console.log(`    ref ${hit}`)
  }
  console.log()
}

