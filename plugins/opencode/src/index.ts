import { Plugin } from "@opencode/plugin"
import { makeStrategy, type Warning } from "./strategy.js"
import { registerTools } from "./tools.js"

export default Plugin.define({
  id: "rift.workspaces",
  async setup(ctx) {
    const listeners = new Set<(warning: Warning) => void>()
    const strategy = makeStrategy(ctx.options, {
      warning: (warning) => {
        console.error(`Rift lifecycle hook failed for ${warning.directory}: ${warning.message}`)
        for (const listener of listeners) listener(warning)
      },
    })
    await ctx.worktree.transform((editor) => editor.add(strategy))
    await registerTools(ctx, listeners)
  },
})
