import { describe, expect, test } from "bun:test"
import { RpcError } from "../src/command.js"
import { makeStrategy } from "../src/strategy.js"

function fixture() {
  const calls: object[] = []
  const children = new Map<string, string[]>([
    ["/project", ["/rifts/one"]],
    ["/rifts/one", ["/rifts/two"]],
    ["/rifts/two", []],
  ])
  const call = async (_executable: string, request: object) => {
    calls.push(request)
    if (!("command" in request)) throw new Error("missing command")
    if (request.command === "create" && "into" in request && "name" in request)
      return `${request.into}/${request.name}`
    if (request.command === "descendants") return ["/rifts/two", "/rifts/one"]
    if (request.command === "list" && "of" in request && typeof request.of === "string")
      return children.get(request.of) ?? []
    return null
  }
  return { calls, children, strategy: makeStrategy({ copyAll: false }, { rpc: call }) }
}

const context = { signal: new AbortController().signal }

describe("Rift workspace strategy", () => {
  test("validates plugin options", () => {
    expect(() => makeStrategy({ executable: "" })).toThrow("non-empty")
    expect(() => makeStrategy({ copyAll: "yes" })).toThrow("boolean")
    expect(() => makeStrategy({ hooks: "yes" })).toThrow("boolean")
    expect(() => makeStrategy({ database: "" })).toThrow("non-empty")
    expect(() => makeStrategy({ unknown: true })).toThrow("Unknown Rift option")
  })

  test("creates at OpenCode's suggested destination", async () => {
    const { calls, strategy } = fixture()
    await expect(
      strategy.create({ sourceDirectory: "/project", directory: "/worktrees/task" }, context),
    ).resolves.toEqual({ directory: "/worktrees/task" })
    expect(calls).toEqual([
      {
        database: undefined,
        command: "create",
        from: "/project",
        into: "/worktrees",
        name: "task",
        copyAll: false,
        hooks: true,
      },
    ])
  })

  test("discovers descendants and removes only leaves", async () => {
    const { calls, children, strategy } = fixture()
    await expect(strategy.list("/project", context)).resolves.toEqual([
      { directory: "/project", type: "root" },
      { directory: "/rifts/two", type: "worktree" },
      { directory: "/rifts/one", type: "worktree" },
    ])
    await expect(strategy.remove({ directory: "/rifts/one", force: true }, context)).rejects.toThrow(
      "Remove this Rift's child workspaces first",
    )
    children.delete("/rifts/two")
    await strategy.remove({ directory: "/rifts/two", force: false }, context)
    expect(calls.at(-1)).toEqual({
      database: undefined,
      command: "remove",
      at: "/rifts/two",
      hooks: true,
    })
  })

  test("keeps OpenCode inventory consistent after post-hook failures", async () => {
    const warnings: Array<{ directory: string; message: string }> = []
    const strategy = makeStrategy(
      {},
      {
        warning: (warning) => warnings.push(warning),
        rpc: async (_executable, request) => {
          if ("command" in request && request.command === "create")
            throw new RpcError({
              code: "hook_failed",
              message: "postcreate failed",
              path: "/worktrees/task",
              hook: "postcreate",
              committed: true,
            })
          if ("command" in request && request.command === "remove")
            throw new RpcError({
              code: "hook_failed",
              message: "postremove failed",
              path: "/rifts/.trash/task",
              hook: "postremove",
              committed: true,
            })
          return []
        },
      },
    )

    await expect(
      strategy.create({ sourceDirectory: "/project", directory: "/worktrees/task" }, context),
    ).resolves.toEqual({ directory: "/worktrees/task" })
    await strategy.remove({ directory: "/worktrees/task", force: false }, context)
    expect(warnings).toEqual([
      { directory: "/worktrees/task", message: "postcreate failed" },
      { directory: "/worktrees/task", message: "postremove failed" },
    ])
  })
})
