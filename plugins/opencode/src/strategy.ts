import { Worktree } from "@opencode/plugin"
import { existsSync } from "node:fs"
import { spawnSync } from "node:child_process"
import path from "node:path"
import { RpcError, rpc } from "./command.js"

interface Options {
  executable: string
  copyAll: boolean
  hooks: boolean
  database?: string
}

export interface Warning {
  directory: string
  message: string
}

export interface ExecutableLookup {
  platform: string
  arch: string
  findOnPath(fileName: string): string | undefined
  exists(filePath: string): boolean
}

interface Runtime {
  warning?: (input: Warning) => void
  rpc?: typeof rpc
  lookup?: ExecutableLookup
}

const hints: Partial<Record<string, string>> = {
  workspace_not_initialized: "Rift source is not initialized; run `rift init` from the project root first",
  initialization_required: "This Rift workspace must be initialized first; run `rift init` from its root folder",
  missing_marker: "This Rift workspace is missing its `.rift` marker; run `rift init` to restore it",
  cow_unavailable:
    "Copy-on-write cloning is unavailable on this volume. Create a Dev Drive and move the project onto it",
  in_use: "Another program is using this workspace. Close it and retry",
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

export function resolveExecutable(configured: string, lookup: ExecutableLookup): string {
  if (lookup.platform !== "win32" || configured.includes("/") || configured.includes("\\")) return configured
  const base = configured.toLowerCase().endsWith(".exe") ? configured.slice(0, -4) : configured
  const exe = lookup.findOnPath(`${base}.exe`)
  if (exe) return exe
  const cmd = lookup.findOnPath(`${base}.cmd`)
  if (!cmd) return configured
  // npm's .cmd shim cannot be spawned without a shell. The binary is <shim dir>\node_modules\rift-snapshot globally and <shim dir>\..\rift-snapshot locally.
  const shimDir = path.win32.dirname(cmd)
  const bundled = (root: string) =>
    path.win32.resolve(shimDir, root, "rift-snapshot", "prebuilds", `windows-${lookup.arch}`, "rift.exe")
  const global = bundled("node_modules")
  const local = bundled("..")
  if (lookup.exists(global)) return global
  return lookup.exists(local) ? local : configured
}

function hostLookup(): ExecutableLookup {
  return {
    platform: process.platform,
    arch: process.arch,
    findOnPath(fileName) {
      const finder = process.platform === "win32" ? "where.exe" : "which"
      const result = spawnSync(finder, [fileName], { encoding: "utf8" })
      if (result.status !== 0) return undefined
      return result.stdout.split(/\r?\n/).find((line) => line.trim())?.trim()
    },
    exists: existsSync,
  }
}

export function makeStrategy(value: Record<string, unknown> = {}, runtime: Runtime = {}) {
  const options = parseOptions(value)
  const executable = resolveExecutable(options.executable, runtime.lookup ?? hostLookup())
  const call = runtime.rpc ?? rpc
  const request = (command: object, signal: AbortSignal) =>
    call(executable, { database: options.database, ...command }, signal)
  const failure = (error: unknown) => {
    const message =
      (error instanceof RpcError && hints[error.code]) || (error instanceof Error ? error.message : String(error))
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
