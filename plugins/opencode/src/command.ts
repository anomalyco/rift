import { spawn } from "node:child_process"

interface Failure {
  code: string
  message: string
  path?: string
  hook?: "precreate" | "postcreate" | "preremove" | "postremove"
  committed?: boolean
}

export class RpcError extends Error implements Failure {
  readonly code: string
  readonly path?: string
  readonly hook?: Failure["hook"]
  readonly committed?: boolean

  constructor(error: Failure) {
    super(error.message)
    this.name = "RiftRpcError"
    this.code = error.code
    this.path = error.path
    this.hook = error.hook
    this.committed = error.committed
  }
}

export function rpc(executable: string, request: object, signal: AbortSignal): Promise<unknown> {
  signal.throwIfAborted()
  return new Promise((resolve, reject) => {
    const grouped = process.platform !== "win32"
    const child = spawn(executable, ["rpc"], {
      detached: grouped,
      stdio: ["pipe", "pipe", "pipe"],
    })
    const chunks: Buffer[] = []
    let size = 0
    let settled = false
    const finish = (result: () => void) => {
      if (settled) return
      settled = true
      signal.removeEventListener("abort", abort)
      result()
    }
    const stop = () => {
      if (!child.pid) return
      try {
        if (grouped) process.kill(-child.pid, "SIGTERM")
        else child.kill()
      } catch {}
    }
    const abort = () => {
      stop()
      finish(() => reject(signal.reason))
    }
    signal.addEventListener("abort", abort, { once: true })
    child.stdout.on("data", (chunk: Buffer) => {
      size += chunk.length
      if (size > 16 * 1024 * 1024) {
        stop()
        finish(() => reject(new Error("Rift RPC response exceeds 16 MiB")))
        return
      }
      chunks.push(chunk)
    })
    child.stderr.pipe(process.stderr, { end: false })
    child.on("error", (error) => finish(() => reject(error)))
    child.stdin.on("error", (error) => finish(() => reject(error)))
    child.on("close", (code) => {
      if (settled) return
      if (code !== 0) return finish(() => reject(new Error(`Rift exited with status ${code}`)))
      let response: unknown
      try {
        response = JSON.parse(Buffer.concat(chunks).toString())
      } catch {
        return finish(() => reject(new Error("Rift returned an invalid RPC response")))
      }
      if (!response || typeof response !== "object" || !("status" in response))
        return finish(() => reject(new Error("Rift returned an invalid RPC response")))
      if (response.status === "ok" && "value" in response) return finish(() => resolve(response.value))
      if (response.status === "error" && "error" in response && response.error && typeof response.error === "object")
        return finish(() => reject(new RpcError(response.error as Failure)))
      finish(() => reject(new Error("Rift returned an invalid RPC response")))
    })
    child.stdin.end(JSON.stringify(request))
  })
}
