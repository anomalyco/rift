import type { Context } from "@opencode/plugin/promise/plugin"

export async function registerTools(ctx: Context, warnings: Map<string, string>) {
  await ctx.tool.transform((editor) => {
    editor.namespace({
      name: "rift",
      description: "Create, inspect, and remove Rift-backed OpenCode worktrees.",
    })
    editor.add({
      name: "create",
      description:
        "Create a Rift worktree from the current session and move this session into it at the next safe boundary.",
      input: {
        type: "object",
        properties: {
          name: { type: "string", description: "Optional workspace name." },
          move: { type: "boolean", description: "Move the current session into the new workspace. Defaults to true." },
        },
        additionalProperties: false,
      },
      output: {
        type: "object",
        properties: {
          directory: { type: "string" },
          warning: { type: "string" },
        },
        required: ["directory"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true, pinned: true },
      async execute(input, tool) {
        const value = input as { name?: string; move?: boolean }
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        const created = await ctx.worktree.create({
          projectID: session.projectID,
          from: session.location.directory,
          name: value.name,
        })
        if (value.move !== false) {
          await ctx.session.move({
            sessionID: session.id,
            directory: created.directory,
            delivery: "steer",
          })
        }
        const warning = warnings.get(created.directory)
        warnings.delete(created.directory)
        return {
          output: warning ? { directory: created.directory, warning } : { directory: created.directory },
          content: warning
            ? `Created ${created.directory}, but a Rift lifecycle hook failed: ${warning}`
            : `Created ${created.directory}${value.move === false ? "." : " and scheduled this session to move there."}`,
        }
      },
    })
    editor.add({
      name: "list",
      description: "Refresh and list Rift-backed worktrees for the current project.",
      input: { type: "object", properties: {}, additionalProperties: false },
      output: {
        type: "object",
        properties: {
          worktrees: {
            type: "array",
            items: {
              type: "object",
              properties: { directory: { type: "string" } },
              required: ["directory"],
              additionalProperties: false,
            },
          },
        },
        required: ["worktrees"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true },
      async execute(_input, tool) {
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        await ctx.worktree.refresh({ projectID: session.projectID })
        const worktrees = (await ctx.worktree.list({ projectID: session.projectID }))
          .filter((entry) => entry.strategy === "rift")
          .map((entry) => ({ directory: entry.directory }))
        return {
          output: { worktrees },
          content: worktrees.length
            ? worktrees.map((entry) => entry.directory).join("\n")
            : "No Rift worktrees found for this project.",
        }
      },
    })
    editor.add({
      name: "remove",
      description: "Remove a Rift-backed worktree from the current project.",
      input: {
        type: "object",
        properties: {
          directory: { type: "string", description: "Absolute workspace path." },
          force: { type: "boolean", description: "Confirm a force-required removal. Defaults to false." },
        },
        required: ["directory"],
        additionalProperties: false,
      },
      output: {
        type: "object",
        properties: { directory: { type: "string" }, warning: { type: "string" } },
        required: ["directory"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true },
      async execute(input, tool) {
        const value = input as { directory: string; force?: boolean }
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        if (session.location.directory === value.directory) {
          const inventory = await ctx.worktree.list({ projectID: session.projectID })
          const destination =
            inventory.find((entry) => entry.strategy === undefined && entry.directory !== value.directory)?.directory ??
            ctx.location.project.canonical
          await ctx.session.move({
            sessionID: session.id,
            directory: destination,
            delivery: "steer",
          })
        }
        await ctx.worktree.remove({
          projectID: session.projectID,
          directory: value.directory,
          force: value.force ?? false,
        })
        const warning = warnings.get(value.directory)
        warnings.delete(value.directory)
        return {
          output: warning ? { directory: value.directory, warning } : { directory: value.directory },
          content: warning
            ? `Removed ${value.directory}, but a Rift lifecycle hook failed: ${warning}`
            : `Removed ${value.directory}.`,
        }
      },
    })
  })
}
