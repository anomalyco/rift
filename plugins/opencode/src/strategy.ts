import { Worktree } from "@opencode/plugin"
import path from "node:path"
import { RpcError, rpc } from "./command.js"

interface Options {
  executable: string
  copyAll: boolean
  hooks: boolean
  database?: string
}

interface Runtime {
  warning?: (input: { directory: string; message: string }) => void
  rpc?: typeof rpc
}

function parseOptions(value: Record<string, unknown>): Options {
  for (const key of Object.keys(value)) {
    if (key !== "executable" && key !== "copyAll" && key !== "hooks" && key !== "database")
      throw new Error(`Unknown Rift option: ${key}`)
  }
  const executable = value.executable ?? "rift"
  const copyAll = value.copyAll ?? false
  const hooks = value.hooks ?? true
  const database = value.database
  if (typeof executable !== "string" || !executable.trim() || executable.includes("\0"))
    throw new Error("Rift executable must be a non-empty command")
  if (typeof copyAll !== "boolean") throw new Error("Rift copyAll must be a boolean")
  if (typeof hooks !== "boolean") throw new Error("Rift hooks must be a boolean")
  if (database !== undefined && (typeof database !== "string" || !database.trim()))
    throw new Error("Rift database must be a non-empty path")
  return { executable, copyAll, hooks, database }
}

export function makeStrategy(value: Record<string, unknown> = {}, runtime: Runtime = {}) {
  const options = parseOptions(value)
  const call = runtime.rpc ?? rpc
  const request = (command: object, signal: AbortSignal) =>
    call(options.executable, { database: options.database, ...command }, signal)
  const failure = (error: unknown) => {
    const message =
      error instanceof RpcError && error.code === "workspace_not_initialized"
        ? "Rift source is not initialized; run `rift init` from the project root first"
        : error instanceof Error
          ? error.message
          : String(error)
    return new Worktree.OperationError({ message })
  }
  const committed = (error: unknown, hook: string) =>
    error instanceof RpcError && error.committed === true && error.hook === hook && error.path
  const paths = (value: unknown) => {
    if (!Array.isArray(value) || value.some((path) => typeof path !== "string"))
      throw new Worktree.OperationError({ message: "Rift returned invalid paths" })
    return value as string[]
  }

  return {
    id: "rift",
    async create(input: { sourceDirectory: string; directory: string; branch?: string }, context: { signal: AbortSignal }) {
      if (input.branch) throw new Worktree.OperationError({ message: "Rift snapshots do not support starting refs" })
      try {
        const directory = await request(
          {
            command: "create",
            from: input.sourceDirectory,
            into: path.dirname(input.directory),
            name: path.basename(input.directory),
            copyAll: options.copyAll,
            hooks: options.hooks,
          },
          context.signal,
        )
        if (typeof directory !== "string") throw new Error("Rift create returned an invalid path")
        return { directory }
      } catch (error) {
        const directory = committed(error, "postcreate")
        if (!directory) throw failure(error)
        runtime.warning?.({ directory, message: (error as Error).message })
        return { directory }
      }
    },
    async remove(input: { directory: string; force: boolean }, context: { signal: AbortSignal }) {
      const children = paths(
        await request({ command: "list", of: input.directory }, context.signal).catch((error) => {
          throw failure(error)
        }),
      )
      if (children.length)
        throw new Worktree.OperationError({ message: "Remove this Rift's child workspaces first" })
      try {
        await request({ command: "remove", at: input.directory, hooks: options.hooks }, context.signal)
      } catch (error) {
        if (!committed(error, "postremove")) throw failure(error)
        runtime.warning?.({ directory: input.directory, message: (error as Error).message })
      }
    },
    async list(sourceDirectory: string, context: { signal: AbortSignal }) {
      try {
        const descendants = paths(await request({ command: "descendants", of: sourceDirectory }, context.signal))
        return [
          { directory: sourceDirectory, type: "root" as const },
          ...descendants.map((directory) => ({ directory, type: "worktree" as const })),
        ]
      } catch (error) {
        if (error instanceof RpcError && error.code === "workspace_not_initialized") return []
        throw failure(error)
      }
    },
  }
}
