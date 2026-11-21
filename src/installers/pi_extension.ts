// ast-bro: pi extension, written by `ast-bro install --target pi`. `ast-bro uninstall` removes it.
//
// A whole-file `read` of a long source file returns its structure map instead of its full text: the
// substitution the Claude Code hook makes. pi's `tool_call` block with a reason behaves like a Claude
// Code PreToolUse block, so the decision is delegated to `ast-bro hook --protocol claude-code`.
import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { isAbsolute, resolve } from "node:path";
import { type ExtensionAPI, isToolCallEventType } from "@earendil-works/pi-coding-agent";

const HOOK_ARGS: string[] = __AST_BRO_HOOK_ARGS__;

function hook(event: object, cwd: string, signal?: AbortSignal): Promise<{ decision?: string; reason?: string } | null> {
	return new Promise((done) => {
		let out = "";
		const child = spawn("ast-bro", HOOK_ARGS, { cwd, signal, stdio: ["pipe", "pipe", "ignore"] });
		const timer = setTimeout(() => child.kill(), 10_000);
		child.stdout.on("data", (chunk) => (out += chunk));
		child.on("error", () => done(null)); // not on PATH, or aborted: the read goes ahead
		child.on("close", (code) => {
			clearTimeout(timer);
			try {
				done(code === 0 ? JSON.parse(out) : null);
			} catch {
				done(null);
			}
		});
		child.stdin.end(JSON.stringify(event));
	});
}

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		if (!isToolCallEventType("read", event)) return;
		const { path, offset, limit } = event.input;
		if (offset !== undefined || limit !== undefined) return; // a ranged read is deliberate
		const expanded = path.startsWith("~/") ? homedir() + path.slice(1) : path;
		const file = isAbsolute(expanded) ? expanded : resolve(ctx.cwd, expanded);
		const answer = await hook(
			{ hook_event_name: "PreToolUse", tool_name: "Read", tool_input: { file_path: file }, cwd: ctx.cwd },
			ctx.cwd,
			ctx.signal,
		);
		if (answer?.decision === "block" && answer.reason) {
			return { block: true, reason: answer.reason };
		}
	});
}
