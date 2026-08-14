import { CString, dlopen, ptr } from "bun:ffi"
import os from "node:os"

const platform = { darwin: "darwin", linux: "linux", win32: "windows" }[os.platform()]
const arch = { arm64: "arm64", x64: "x64" }[os.arch()]
if (!platform || !arch) throw new Error(`Unsupported Rift platform: ${os.platform()}-${os.arch()}`)

let libraryPath
if (platform === "linux" && arch === "x64") {
  libraryPath = (await import("../prebuilds/linux-x64/librift_ffi.so", { with: { type: "file" } })).default
} else if (platform === "darwin" && arch === "x64") {
  libraryPath = (await import("../prebuilds/darwin-x64/librift_ffi.dylib", { with: { type: "file" } })).default
} else if (platform === "darwin" && arch === "arm64") {
  libraryPath = (await import("../prebuilds/darwin-arm64/librift_ffi.dylib", { with: { type: "file" } })).default
} else if (platform === "windows" && arch === "x64") {
  libraryPath = (await import("../prebuilds/windows-x64/rift_ffi.dll", { with: { type: "file" } })).default
} else {
  throw new Error(`Unsupported Rift platform: ${platform}-${arch}`)
}

const { symbols } = dlopen(libraryPath, {
  rift_ffi_call: { args: ["ptr"], returns: "ptr" },
  rift_ffi_free: { args: ["ptr"], returns: "void" },
})
const encoder = new TextEncoder()

function call(request) {
  const input = encoder.encode(`${JSON.stringify(request)}\0`)
  const output = symbols.rift_ffi_call(ptr(input))
  if (!output) throw new Error("Rift native library returned no response")
  let response
  try {
    response = JSON.parse(new CString(output).toString())
  } finally {
    symbols.rift_ffi_free(output)
  }
  if (response.status === "error") throw new RiftError(response.error)
  return response.value
}

export class RiftError extends Error {
  constructor({ code, message, path }) {
    super(message)
    this.name = "RiftError"
    this.code = code
    this.path = path
  }
}

export function init({ at = process.cwd(), database } = {}) {
  return call({ command: "init", at, database })
}

export function create({ from = process.cwd(), name, into, copyAll, hooks, database } = {}) {
  return call({ command: "create", from, name, into, copyAll, hooks, database })
}

export function remove({ at = process.cwd(), all = false, hooks, database } = {}) {
  const result = call({ command: "remove", at, all, hooks, database })
  return all ? result : undefined
}

export function list({ of = process.cwd(), database } = {}) {
  return call({ command: "list", of, database })
}

export function ancestors({ of = process.cwd(), database } = {}) {
  return call({ command: "ancestors", of, database })
}

export function gc({ database } = {}) {
  return call({ command: "gc", database })
}
