import { afterAll, beforeAll, expect, test } from "bun:test"
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import plugin from "../src/index.js"

interface Tool {
  name: string
  execute: (input: unknown, context: { sessionID: string }) => Promise<{ output: unknown; content: string }>
}

interface Strategy {
  create: (input: { sourceDirectory: string; directory: string }, context: { signal: AbortSignal }) => Promise<{ directory: string }>
  remove: (input: { directory: string; force: boolean }, context: { signal: AbortSignal }) => Promise<void>
}

let temp: string

beforeAll(async () => {
  temp = await mkdtemp(join(tmpdir(), "opencode-rift-plugin-"))
})

afterAll(async () => {
  await rm(temp, { recursive: true, force: true })
})

async function fakeRift(failure?: { command: string; hook: string; path: string }) {
  const executable = join(temp, `rift-${Math.random().toString(36).slice(2)}`)
  await writeFile(
    executable,
    `#!/usr/bin/env node
const failure = ${JSON.stringify(failure ?? null)}
let input = ""
process.stdin.on("data", (chunk) => (input += chunk))
process.stdin.on("end", () => {
  const request = JSON.parse(input)
  if (failure && request.command === failure.command) {
    process.stderr.write("ERR_PNPM_FETCH_401 token expired\\n")
    const error = { code: "hook_failed", message: failure.hook + " hook failed", path: failure.path, hook: failure.hook, committed: true }
    process.stdout.write(JSON.stringify({ status: "error", error }))
    return
  }
  const value = request.command === "create" ? request.into + "/" + request.name : request.command === "list" ? [] : null
  process.stdout.write(JSON.stringify({ status: "ok", value }))
})
`,
  )
  await chmod(executable, 0o755)
  return executable
}

async function setup(executable: string) {
  const tools = new Map<string, Tool>()
  let strategy: Strategy | undefined
  const signal = new AbortController().signal
  const ctx = {
    options: { executable },
    session: {
      get: async () => ({ projectID: "project", location: { directory: "/project" } }),
    },
    worktree: {
      transform: async (callback: (editor: { add: (definition: Strategy) => void }) => void) => {
        callback({ add: (definition) => (strategy = definition) })
      },
      list: async () => [{ directory: "/project" }],
      create: (input: { from: string; name: string }) =>
        strategy!.create({ sourceDirectory: input.from, directory: `/worktrees/${input.name}` }, { signal }),
      remove: (input: { directory: string; force: boolean }) => strategy!.remove(input, { signal }),
    },
    tool: {
      transform: async (callback: (editor: { namespace: () => void; add: (tool: Tool) => void }) => void) => {
        callback({ namespace: () => {}, add: (tool) => tools.set(tool.name, tool) })
      },
    },
  }
  await plugin.setup(ctx as never)
  return tools
}

test("rift.create reports a failed postcreate hook to the agent", async () => {
  const tools = await setup(await fakeRift({ command: "create", hook: "postcreate", path: "/worktrees/task" }))
  const result = await tools.get("create")!.execute({ name: "task" }, { sessionID: "session" })
  expect(result.output).toEqual({
    directory: "/worktrees/task",
    warning: "postcreate hook failed: ERR_PNPM_FETCH_401 token expired",
  })
  expect(result.content).toBe(
    "Created /worktrees/task. Use opencode.session_move to move a session into it.\n\n" +
      "Warning: postcreate hook failed: ERR_PNPM_FETCH_401 token expired",
  )
})

test("rift.remove reports a failed postremove hook to the agent", async () => {
  const tools = await setup(await fakeRift({ command: "remove", hook: "postremove", path: "/rifts/.trash/task" }))
  const result = await tools.get("remove")!.execute({ directory: "/worktrees/task" }, { sessionID: "session" })
  expect(result.output).toEqual({
    directory: "/worktrees/task",
    warning: "postremove hook failed: ERR_PNPM_FETCH_401 token expired",
  })
  expect(result.content).toBe(
    "Removed /worktrees/task.\n\nWarning: postremove hook failed: ERR_PNPM_FETCH_401 token expired",
  )
})

test("successful hooks add no warning", async () => {
  const tools = await setup(await fakeRift())
  const result = await tools.get("create")!.execute({ name: "task" }, { sessionID: "session" })
  expect(result).toEqual({
    output: { directory: "/worktrees/task" },
    content: "Created /worktrees/task. Use opencode.session_move to move a session into it.",
  })
})
