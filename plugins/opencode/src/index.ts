import { Plugin } from "@opencode/plugin"
import { makeStrategy } from "./strategy.js"
import { registerTools } from "./tools.js"

export default Plugin.define({
  id: "rift.worktrees",
  async setup(ctx) {
    const strategy = makeStrategy(ctx.options, {
      warning: ({ directory, message }) => {
        console.error(`Rift lifecycle hook failed for ${directory}: ${message}`)
      },
    })
    await ctx.worktree.transform((editor) => editor.add(strategy))
    await registerTools(ctx)
  },
})
