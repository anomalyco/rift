import { Worktree } from "@opencode/plugin"
import path from "node:path"

interface Rift {
  create(options: {
    from: string
    name: string
    into: string
    copyAll: boolean
    hooks: boolean
    database?: string
  }): string
  remove(options: { at: string; hooks: boolean; database?: string }): void
  list(options: { of: string; database?: string }): string[]
}

interface Options {
  copyAll: boolean
  hooks: boolean
  database?: string
}

function parseOptions(value: Record<string, unknown>): Options {
  for (const key of Object.keys(value)) {
    if (key !== "copyAll" && key !== "hooks" && key !== "database") throw new Error(`Unknown Rift option: ${key}`)
  }
  const copyAll = value.copyAll ?? false
  const hooks = value.hooks ?? true
  const database = value.database
  if (typeof copyAll !== "boolean") throw new Error("Rift copyAll must be a boolean")
  if (typeof hooks !== "boolean") throw new Error("Rift hooks must be a boolean")
  if (database !== undefined && (typeof database !== "string" || !database.trim()))
    throw new Error("Rift database must be a non-empty path")
  return { copyAll, hooks, database }
}

export function makeStrategy(rift: Rift, value: Record<string, unknown> = {}) {
  const options = parseOptions(value)
  const settings = { database: options.database }
  const failure = (error: unknown) => {
    const message =
      error !== null && typeof error === "object" && "code" in error && error.code === "workspace_not_initialized"
        ? "Rift source is not initialized; run `rift init` from the project root first"
        : error instanceof Error
          ? error.message
          : String(error)
    return new Worktree.OperationError({ message })
  }
  const run = <T>(operation: () => T) => {
    try {
      return operation()
    } catch (error) {
      throw failure(error)
    }
  }

  return {
    id: "rift",
    async create(input: { sourceDirectory: string; directory: string; branch?: string }, context: { signal: AbortSignal }) {
      context.signal.throwIfAborted()
      if (input.branch) throw new Worktree.OperationError({ message: "Rift snapshots do not support starting refs" })
      const directory = run(() =>
        rift.create({
          ...settings,
          from: input.sourceDirectory,
          into: path.dirname(input.directory),
          name: path.basename(input.directory),
          copyAll: options.copyAll,
          hooks: options.hooks,
        }),
      )
      return { directory }
    },
    async remove(input: { directory: string; force: boolean }, context: { signal: AbortSignal }) {
      context.signal.throwIfAborted()
      const children = run(() => rift.list({ ...settings, of: input.directory }))
      if (children.length)
        throw new Worktree.OperationError({ message: "Remove this Rift's child workspaces first" })
      run(() => rift.remove({ ...settings, at: input.directory, hooks: options.hooks }))
    },
    async list(sourceDirectory: string, context: { signal: AbortSignal }) {
      context.signal.throwIfAborted()
      const entries: Array<{ directory: string; type: "root" | "worktree" }> = []
      const queue = [sourceDirectory]
      const seen = new Set(queue)
      while (queue.length) {
        const directory = queue.shift()!
        let children: string[]
        try {
          children = rift.list({ ...settings, of: directory })
        } catch (error) {
          if (
            directory === sourceDirectory &&
            error !== null &&
            typeof error === "object" &&
            "code" in error &&
            error.code === "workspace_not_initialized"
          )
            return []
          throw failure(error)
        }
        entries.push({ directory, type: directory === sourceDirectory ? "root" : "worktree" })
        for (const child of children) {
          if (seen.has(child)) continue
          seen.add(child)
          queue.push(child)
        }
        context.signal.throwIfAborted()
      }
      return entries
    },
  }
}
