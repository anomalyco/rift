import { expect, test } from "bun:test"
import { spawn } from "node:child_process"
import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { fileURLToPath } from "node:url"
import { RpcError, rpc } from "../src/command.js"

test("cancellation stops an in-flight RPC process", async () => {
  const temp = await mkdtemp(join(tmpdir(), "opencode-rift-rpc-"))
  const executable = join(temp, "rift")
  await writeFile(executable, "#!/usr/bin/env node\nsetTimeout(() => {}, 60000)\n")
  await chmod(executable, 0o755)
  const controller = new AbortController()
  const reason = new Error("cancelled")
  const pending = rpc(executable, {}, controller.signal)
  const timer = setTimeout(() => controller.abort(reason), 50)
  try {
    await expect(pending).rejects.toBe(reason)
  } finally {
    clearTimeout(timer)
    await rm(temp, { recursive: true, force: true })
  }
})

test("repeated RPCs complete with a disconnected host stderr", async () => {
  const temp = await mkdtemp(join(tmpdir(), "opencode-rift-rpc-"))
  const executable = join(temp, "rift")
  await writeFile(executable, `#!/usr/bin/env node
let count = 0
const timer = setInterval(() => {
  process.stderr.write("hook output\\n".repeat(10000))
  if (++count < 20) return
  clearInterval(timer)
  process.stdout.write(JSON.stringify({ status: "ok", value: "/workspace" }))
}, 10)
`)
  await chmod(executable, 0o755)
  await writeFile(join(temp, "runner.ts"), `
import { rpc } from ${JSON.stringify(fileURLToPath(new URL("../src/command.ts", import.meta.url)))}
for (let index = 0; index < 3; index++) {
  console.log(await rpc(${JSON.stringify(executable)}, {}, AbortSignal.timeout(1500)))
}
`)
  try {
    const result = await new Promise<{ code: number | null; stdout: string }>((resolve, reject) => {
      const child = spawn(process.execPath, [join(temp, "runner.ts")], { stdio: ["ignore", "pipe", "pipe"] })
      let stdout = ""
      child.stderr.destroy()
      child.stdout.on("data", (chunk: Buffer) => {
        stdout += chunk.toString()
      })
      child.on("error", reject)
      child.on("close", (code) => resolve({ code, stdout }))
    })
    expect(result.code).toBe(0)
    expect(result.stdout.trim().split("\n")).toEqual(["/workspace", "/workspace", "/workspace"])
  } finally {
    await rm(temp, { recursive: true, force: true })
  }
})

test("failed RPCs include a bounded stderr tail", async () => {
  const temp = await mkdtemp(join(tmpdir(), "opencode-rift-rpc-"))
  const executable = join(temp, "rift")
  await writeFile(
    executable,
    '#!/usr/bin/env node\nprocess.stderr.write("x".repeat(20000) + "hook failed\\n", () => process.exit(1))\n',
  )
  await chmod(executable, 0o755)
  try {
    await expect(rpc(executable, {}, AbortSignal.timeout(1500))).rejects.toThrow(
      `Rift exited with status 1: ${"x".repeat(8 * 1024 - "hook failed\n".length)}hook failed`,
    )
  } finally {
    await rm(temp, { recursive: true, force: true })
  }
})

test("structured hook failures include hook output", async () => {
  const temp = await mkdtemp(join(tmpdir(), "opencode-rift-rpc-"))
  const executable = join(temp, "rift")
  const failure = {
    code: "hook_failed",
    message: "postcreate hook failed at /workspace: `pnpm install` exited with exit status: 1",
    path: "/workspace",
    hook: "postcreate",
    committed: true,
  }
  await writeFile(
    executable,
    `#!/usr/bin/env node
process.stderr.write("ERR_PNPM_FETCH_401 token expired\\n")
process.stdout.write(${JSON.stringify(JSON.stringify({ status: "error", error: failure }))})
`,
  )
  await chmod(executable, 0o755)
  try {
    const error = await rpc(executable, {}, AbortSignal.timeout(1500)).catch((error: unknown) => error)
    expect(error).toBeInstanceOf(RpcError)
    expect({ ...(error as RpcError), message: (error as RpcError).message }).toEqual({
      name: "RiftRpcError",
      code: "hook_failed",
      message: `${failure.message}: ERR_PNPM_FETCH_401 token expired`,
      path: "/workspace",
      hook: "postcreate",
      committed: true,
    })
  } finally {
    await rm(temp, { recursive: true, force: true })
  }
})

test("RPCs finish when hook processes keep stderr open", async () => {
  const temp = await mkdtemp(join(tmpdir(), "opencode-rift-rpc-"))
  const executable = join(temp, "rift")
  const pidfile = join(temp, "daemon.pid")
  await writeFile(
    executable,
    `#!/usr/bin/env node
const { spawn } = require("node:child_process")
const daemon = spawn("sleep", ["30"], { stdio: ["ignore", "ignore", "inherit"] })
require("node:fs").writeFileSync(${JSON.stringify(pidfile)}, String(daemon.pid))
daemon.unref()
process.stderr.write("compose: port 5432 in use\\n", () => {
  process.stdout.write(${JSON.stringify(JSON.stringify({ status: "error", error: { code: "hook_failed", message: "postcreate hook failed" } }))})
})
`,
  )
  await chmod(executable, 0o755)
  try {
    await expect(rpc(executable, {}, AbortSignal.timeout(3000))).rejects.toThrow(
      "postcreate hook failed: compose: port 5432 in use",
    )
  } finally {
    process.kill(Number(await readFile(pidfile, "utf8")))
    await rm(temp, { recursive: true, force: true })
  }
})

test("signal deaths name the signal", async () => {
  const temp = await mkdtemp(join(tmpdir(), "opencode-rift-rpc-"))
  const executable = join(temp, "rift")
  await writeFile(
    executable,
    '#!/usr/bin/env node\nprocess.stderr.write("fatal runtime error\\n", () => process.kill(process.pid, "SIGKILL"))\n',
  )
  await chmod(executable, 0o755)
  try {
    await expect(rpc(executable, {}, AbortSignal.timeout(3000))).rejects.toThrow(
      "Rift was terminated by SIGKILL: fatal runtime error",
    )
  } finally {
    await rm(temp, { recursive: true, force: true })
  }
})
