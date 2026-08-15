// jjfx-pi-lifecycle-extension:v1
// @ts-nocheck - jjfx embeds this file; Pi supplies Node and extension types at runtime.

import { appendFile, mkdir } from "node:fs/promises";
import { dirname, join } from "node:path";

const EVENT_VERSION = 1;

function eventLogPath() {
	const xdgState = process.env.XDG_STATE_HOME?.trim();
	if (xdgState) {
		return join(xdgState, "jjfx", "events.jsonl");
	}

	const home = process.env.HOME?.trim();
	return home
		? join(home, ".local", "state", "jjfx", "events.jsonl")
		: undefined;
}

async function appendLifecycleEvent(hookEventName, ctx) {
	const logPath = eventLogPath();
	if (!logPath) {
		return;
	}

	const event = {
		jjfx_event_version: EVENT_VERSION,
		hook_event_name: hookEventName,
		agent_kind: "pi",
		session_id: ctx.sessionManager.getSessionId(),
		cwd: ctx.cwd,
	};

	try {
		await mkdir(dirname(logPath), { recursive: true });
		await appendFile(logPath, `${JSON.stringify(event)}\n`, {
			encoding: "utf8",
			flag: "a",
		});
	} catch {
		// Lifecycle reporting must never interrupt the Pi session.
	}
}

export default function jjfxLifecycle(pi) {
	pi.on("session_start", async (_event, ctx) => {
		await appendLifecycleEvent("SessionStart", ctx);
	});
	pi.on("agent_start", async (_event, ctx) => {
		await appendLifecycleEvent("UserPromptSubmit", ctx);
	});
	pi.on("turn_start", async (_event, ctx) => {
		await appendLifecycleEvent("UserPromptSubmit", ctx);
	});
	pi.on("agent_settled", async (_event, ctx) => {
		await appendLifecycleEvent("Stop", ctx);
	});
	pi.on("session_shutdown", async (_event, ctx) => {
		await appendLifecycleEvent("SessionEnd", ctx);
	});
}
