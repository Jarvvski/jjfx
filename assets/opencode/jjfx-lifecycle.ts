// jjfx-opencode-lifecycle-plugin:v1
import { appendFile, mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";

function path() {
  return join(process.env.XDG_STATE_HOME || join(process.env.HOME || "", ".local", "state"), "jjfx", "events.jsonl");
}

async function write(name: string, sessionID: string, cwd: string) {
  try {
    const target = path();
    await mkdir(dirname(target), { recursive: true });
    await appendFile(target, JSON.stringify({ jjfx_event_version: 1, hook_event_name: name, agent_kind: "opencode", session_id: sessionID, cwd }) + "\n");
  } catch {
    // Observation must never interrupt an OpenCode session.
  }
}

export default async function jjfxLifecycle({ directory }: { directory: string }) {
  return {
    event: async ({ event }: { event: { type: string; properties: any } }) => {
      const id = event.properties?.sessionID || event.properties?.info?.id;
      if (!id) return;
      if (event.type === "session.created") await write("SessionStart", id, directory);
      if (event.type === "session.status" && event.properties.status?.type !== "idle") await write("UserPromptSubmit", id, directory);
      if (event.type === "session.idle") await write("Stop", id, directory);
      if (event.type === "session.deleted") await write("SessionEnd", id, directory);
      if (event.type === "permission.asked") await write("PermissionRequest", id, directory);
    },
  };
}
