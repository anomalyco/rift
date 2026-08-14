import { dlopen, toString } from "node:ffi"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { fileURLToPath } from "node:url"

const platform = { darwin: "darwin", linux: "linux", win32: "windows" }[os.platform()]
const arch = { arm64: "arm64", x64: "x64" }[os.arch()]
if (!platform || !arch) throw new Error(`Unsupported Rift platform: ${os.platform()}-${os.arch()}`)

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)))
const libraryName = platform === "windows" ? "rift_ffi.dll" : platform === "darwin" ? "librift_ffi.dylib" : "librift_ffi.so"
const libraryPath = path.join(root, "prebuilds", `${platform}-${arch}`, libraryName)
if (!fs.existsSync(libraryPath)) throw new Error(`Unable to locate the Rift Node library for ${platform}-${arch}. Reinstall rift-snapshot.`)

const { functions } = dlopen(libraryPath, {
  rift_ffi_call: { parameters: ["string"], result: "pointer" },
  rift_ffi_free: { parameters: ["pointer"], result: "void" },
})

function call(request) {
  const output = functions.rift_ffi_call(JSON.stringify(request))
  if (!output) throw new Error("Rift native library returned no response")
  let response
  try {
    response = JSON.parse(toString(output))
  } finally {
    functions.rift_ffi_free(output)
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
