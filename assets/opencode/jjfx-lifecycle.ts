// jjfx-opencode-lifecycle-plugin:v1
import { appendFile, mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";

function path() {
  return join(process.env.XDG_STATE_HOME || join(process.env.HOME || "", ".local", "state"), "jjfx", "events.jsonl");
}

// Session id -> the session that owns it, learned when a session is created.
// Only the created event names a parent, and every later event for that session
// names only the session, so the parent has to be remembered for each line to
// stay self-describing. A child session is a session with a parent; a root
// session (one no other session dispatched) has none.
const parents = new Map<string, string>();

function parentOf(properties: any): string | undefined {
  return properties?.parentID ?? properties?.info?.parentID;
}

async function write(name: string, sessionID: string, cwd: string) {
  try {
    const target = path();
    await mkdir(dirname(target), { recursive: true });
    const parentID = parents.get(sessionID);
    const event = {
      jjfx_event_version: 1,
      hook_event_name: name,
      agent_kind: "opencode",
      session_id: sessionID,
      cwd,
      ...(parentID ? { parent_id: parentID } : {}),
    };
    await appendFile(target, JSON.stringify(event) + "\n");
  } catch {
    // Observation must never interrupt an OpenCode session.
  }
}

export default async function jjfxLifecycle({ directory }: { directory: string }) {
  return {
    event: async ({ event }: { event: { type: string; properties: any } }) => {
      const properties = event.properties ?? {};
      const id = properties.sessionID ?? properties.info?.id;
      if (!id) return;
      if (event.type === "session.created") {
        // Remember the session's parent, if it has one, so the child's whole
        // lifecycle is reported against the session that dispatched it.
        const parentID = parentOf(properties);
        if (parentID) parents.set(id, parentID);
        await write("SessionStart", id, directory);
      }
      if (event.type === "session.status" && properties.status?.type !== "idle") await write("UserPromptSubmit", id, directory);
      if (event.type === "session.idle") await write("Stop", id, directory);
      if (event.type === "session.deleted") {
        await write("SessionEnd", id, directory);
        parents.delete(id);
      }
      if (event.type === "permission.asked") await write("PermissionRequest", id, directory);
      // The execution lifecycle is the event pair a v2 session actually emits
      // for a turn starting and finishing; the status/idle pair above is what
      // older opencode reports. Both fold to the same states.
      if (event.type === "session.execution.started") await write("UserPromptSubmit", id, directory);
      if (event.type === "session.execution.succeeded" || event.type === "session.execution.failed") await write("Stop", id, directory);
      if (event.type === "session.execution.interrupted") await write("Interrupt", id, directory);
    },
  };
}
