import type { Context } from "@opencode/plugin/promise/plugin"
import path from "node:path"
import type { Warning } from "./strategy.js"

type Listeners = Set<(warning: Warning) => void>

function contains(parent: string, child: string) {
  const relative = path.relative(parent, child)
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative))
}

// The strategy raises committed hook failures inside the host's worktree call,
// which reports success, so the tool listens for them while that call runs.
async function watch<T>(listeners: Listeners, run: () => Promise<T>) {
  const warnings: Warning[] = []
  const listener = (warning: Warning) => warnings.push(warning)
  listeners.add(listener)
  try {
    const result = await run()
    return { result, warning: (directory: string) => warnings.find((entry) => entry.directory === directory)?.message }
  } finally {
    listeners.delete(listener)
  }
}

function report(directory: string, content: string, warning: string | undefined) {
  return warning
    ? { output: { directory, warning }, content: `${content}\n\nWarning: ${warning}` }
    : { output: { directory }, content }
}

export async function registerTools(ctx: Context, listeners: Listeners) {
  await ctx.tool.transform((editor) => {
    editor.namespace({
      name: "rift",
      description: "Create, inspect, and remove Rift workspaces for the current OpenCode project.",
    })
    editor.add({
      name: "create",
      description: "Create a Rift workspace from the current session's workspace.",
      input: {
        type: "object",
        properties: {
          name: { type: "string", description: "Optional workspace name." },
        },
        additionalProperties: false,
      },
      output: {
        type: "object",
        properties: {
          directory: { type: "string" },
          warning: { type: "string", description: "Set when a postcreate hook failed after creation." },
        },
        required: ["directory"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true, pinned: true },
      async execute(input, tool) {
        const value = input as { name?: string }
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        const inventory = await ctx.worktree.list({ projectID: session.projectID })
        const source = inventory
          .filter((entry) => contains(entry.directory, session.location.directory))
          .toSorted((left, right) => right.directory.length - left.directory.length)[0]
        if (!source) throw new Error(`Current session is not inside a known workspace: ${session.location.directory}`)
        const { result: created, warning } = await watch(listeners, () =>
          ctx.worktree.create({
            projectID: session.projectID,
            from: source.directory,
            name: value.name,
          }),
        )
        return report(
          created.directory,
          `Created ${created.directory}. Use opencode.session_move to move a session into it.`,
          warning(created.directory),
        )
      },
    })
    editor.add({
      name: "list",
      description: "Refresh and list Rift workspaces for the current project.",
      input: { type: "object", properties: {}, additionalProperties: false },
      output: {
        type: "object",
        properties: {
          workspaces: {
            type: "array",
            items: {
              type: "object",
              properties: { directory: { type: "string" } },
              required: ["directory"],
              additionalProperties: false,
            },
          },
        },
        required: ["workspaces"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true },
      async execute(_input, tool) {
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        await ctx.worktree.refresh({ projectID: session.projectID })
        const workspaces = (await ctx.worktree.list({ projectID: session.projectID }))
          .filter((entry) => entry.strategy === "rift")
          .map((entry) => ({ directory: entry.directory }))
        return {
          output: { workspaces },
          content: workspaces.length
            ? workspaces.map((entry) => entry.directory).join("\n")
            : "No Rift workspaces found for this project.",
        }
      },
    })
    editor.add({
      name: "remove",
      description:
        "Remove a Rift workspace from the current project. Refuses the workspace this session is in and workspaces that still have child workspaces.",
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
        properties: {
          directory: { type: "string" },
          warning: { type: "string", description: "Set when a postremove hook failed after removal." },
        },
        required: ["directory"],
        additionalProperties: false,
      },
      options: { namespace: "rift", codemode: true },
      async execute(input, tool) {
        const value = input as { directory: string }
        const session = await ctx.session.get({ sessionID: tool.sessionID })
        if (contains(value.directory, session.location.directory))
          throw new Error("Move this session out of the Rift before removing it")
        const { warning } = await watch(listeners, () =>
          ctx.worktree.remove({
            projectID: session.projectID,
            directory: value.directory,
            force: false,
          }),
        )
        return report(value.directory, `Removed ${value.directory}.`, warning(value.directory))
      },
    })
  })
}
