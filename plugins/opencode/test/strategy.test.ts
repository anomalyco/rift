import { describe, expect, test } from "bun:test"
import { makeStrategy } from "../src/strategy.js"

function fixture() {
  const calls: Array<{ operation: string; options: unknown }> = []
  const children = new Map<string, string[]>([
    ["/project", ["/rifts/one"]],
    ["/rifts/one", ["/rifts/two"]],
    ["/rifts/two", []],
  ])
  const rift = {
    create(options: Parameters<Parameters<typeof makeStrategy>[0]["create"]>[0]) {
      calls.push({ operation: "create", options })
      return `${options.into}/${options.name}`
    },
    remove(options: Parameters<Parameters<typeof makeStrategy>[0]["remove"]>[0]) {
      calls.push({ operation: "remove", options })
    },
    list(options: Parameters<Parameters<typeof makeStrategy>[0]["list"]>[0]) {
      calls.push({ operation: "list", options })
      return children.get(options.of) ?? []
    },
  }
  return { calls, children, rift, strategy: makeStrategy(rift, { copyAll: false }) }
}

const context = { signal: new AbortController().signal }

describe("Rift worktree strategy", () => {
  test("validates plugin options", () => {
    const { rift } = fixture()
    expect(() => makeStrategy(rift, { copyAll: "yes" })).toThrow("boolean")
    expect(() => makeStrategy(rift, { hooks: "yes" })).toThrow("boolean")
    expect(() => makeStrategy(rift, { database: "" })).toThrow("non-empty")
    expect(() => makeStrategy(rift, { unknown: true })).toThrow("Unknown Rift option")
  })

  test("creates at OpenCode's suggested destination", async () => {
    const { calls, strategy } = fixture()
    await expect(
      strategy.create({ sourceDirectory: "/project", directory: "/worktrees/task" }, context),
    ).resolves.toEqual({ directory: "/worktrees/task" })
    expect(calls).toEqual([
      {
        operation: "create",
        options: {
          database: undefined,
          from: "/project",
          into: "/worktrees",
          name: "task",
          copyAll: false,
          hooks: true,
        },
      },
    ])
  })

  test("discovers descendants and removes only leaves", async () => {
    const { calls, children, strategy } = fixture()
    await expect(strategy.list("/project", context)).resolves.toEqual([
      { directory: "/project", type: "root" },
      { directory: "/rifts/one", type: "worktree" },
      { directory: "/rifts/two", type: "worktree" },
    ])
    await expect(strategy.remove({ directory: "/rifts/one", force: true }, context)).rejects.toThrow(
      "Remove this Rift's child workspaces first",
    )
    children.delete("/rifts/two")
    await strategy.remove({ directory: "/rifts/two", force: false }, context)
    expect(calls.at(-1)).toEqual({
      operation: "remove",
      options: { database: undefined, at: "/rifts/two", hooks: true },
    })
  })
})
