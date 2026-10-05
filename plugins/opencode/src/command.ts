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
    let tail = Buffer.alloc(0)
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
    // The background host's stderr may have no reader, so drain it here rather
    // than forwarding it there.
    child.stderr.on("data", (chunk: Buffer) => {
      tail = Buffer.concat([tail, chunk]).subarray(-8 * 1024)
    })
    // Hook output only reaches the host through these messages, and hook
    // failures arrive as structured errors from a zero exit.
    const describe = (message: string) => {
      const detail = tail.toString().trim()
      return detail ? `${message}: ${detail}` : message
    }
    child.on("error", (error) => finish(() => reject(error)))
    child.stdin.on("error", (error) => finish(() => reject(error)))
    const complete = (code: number | null, killed: NodeJS.Signals | null) => {
      if (settled) return
      if (killed) return finish(() => reject(new Error(describe(`Rift was terminated by ${killed}`))))
      if (code !== 0) return finish(() => reject(new Error(describe(`Rift exited with status ${code}`))))
      const invalid = () => finish(() => reject(new Error(describe("Rift returned an invalid RPC response"))))
      let response: unknown
      try {
        response = JSON.parse(Buffer.concat(chunks).toString())
      } catch {
        return invalid()
      }
      if (!response || typeof response !== "object" || !("status" in response)) return invalid()
      if (response.status === "ok" && "value" in response) return finish(() => resolve(response.value))
      if (response.status === "error" && "error" in response && response.error && typeof response.error === "object") {
        const failure = response.error as Failure
        return finish(() => reject(new RpcError({ ...failure, message: describe(failure.message) })))
      }
      invalid()
    }
    // Processes started by hooks inherit stderr and can hold it open long after
    // Rift exits, so close may never fire. Once Rift has exited and stdout has
    // ended, give stderr a moment to flush and settle without waiting for close.
    let exited: [number | null, NodeJS.Signals | null] | undefined
    let ended = false
    const drained = () => {
      if (exited && ended) setTimeout(complete, 100, ...exited).unref()
    }
    child.on("exit", (code, killed) => {
      exited = [code, killed]
      drained()
    })
    child.stdout.on("end", () => {
      ended = true
      drained()
    })
    child.on("close", complete)
    child.stdin.end(JSON.stringify(request))
  })
}
