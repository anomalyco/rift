import { expect, test } from "bun:test"
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { rpc } from "../src/command.js"

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
