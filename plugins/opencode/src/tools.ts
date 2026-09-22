import type { Context } from "@opencode/plugin/promise/plugin"
import path from "node:path"

function contains(parent: string, child: string) {
  const relative = path.relative(parent, child)
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative))
}

export async function registerTools(ctx: Context) {
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
        },
        required: ["directory"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true, pinned: true },
      async execute(input, tool) {
        const value = input as { name?: string; move?: boolean }
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        const inventory = await ctx.worktree.list({ projectID: session.projectID })
        const source = inventory
          .filter((entry) => contains(entry.directory, session.location.directory))
          .toSorted((left, right) => right.directory.length - left.directory.length)[0]
        if (!source) throw new Error(`Current session is not inside a known workspace: ${session.location.directory}`)
        const created = await ctx.worktree.create({
          projectID: session.projectID,
          from: source.directory,
          name: value.name,
        })
        let moveError: unknown
        if (value.move !== false)
          await ctx.session
            .move({
              sessionID: session.id,
              directory: created.directory,
              delivery: "steer",
            })
            .catch((error) => {
              moveError = error
            })
        return {
          output: { directory: created.directory },
          content: moveError
            ? `Created ${created.directory}, but could not move this session: ${moveError instanceof Error ? moveError.message : String(moveError)}`
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
        },
        required: ["directory"],
        additionalProperties: false,
      },
      output: {
        type: "object",
        properties: { directory: { type: "string" } },
        required: ["directory"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true },
      async execute(input, tool) {
        const value = input as { directory: string }
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        if (contains(value.directory, session.location.directory)) {
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
          force: false,
        })
        return {
          output: { directory: value.directory },
          content: `Removed ${value.directory}.`,
        }
      },
    })
  })
}
