import { execFile } from "node:child_process"

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
    const child = execFile(
      executable,
      ["rpc"],
      { signal, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 },
      (error, stdout, stderr) => {
        if (signal.aborted) return reject(signal.reason)
        if (error) return reject(new Error(stderr.trim() || error.message))
        let response: unknown
        try {
          response = JSON.parse(stdout)
        } catch {
          return reject(new Error("Rift returned an invalid RPC response"))
        }
        if (!response || typeof response !== "object" || !("status" in response))
          return reject(new Error("Rift returned an invalid RPC response"))
        if (response.status === "ok" && "value" in response) return resolve(response.value)
        if (response.status === "error" && "error" in response && response.error && typeof response.error === "object")
          return reject(new RpcError(response.error as Failure))
        reject(new Error("Rift returned an invalid RPC response"))
      },
    )
    child.stderr?.pipe(process.stderr, { end: false })
    child.stdin?.end(JSON.stringify(request))
  })
}
