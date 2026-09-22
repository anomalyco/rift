import { Plugin } from "@opencode/plugin"
import * as rift from "rift-snapshot"
import { makeStrategy } from "./strategy.js"

export default Plugin.define({
  id: "rift.worktrees",
  async setup(ctx) {
    const strategy = makeStrategy(rift, ctx.options)
    await ctx.worktree.transform((editor) => editor.add(strategy))
  },
})
