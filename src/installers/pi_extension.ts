// ast-bro: pi extension, written by `ast-bro install --target pi`. `ast-bro uninstall` removes it.
//
// A whole-file `read` of a long source file returns its structure map instead of its full text: the
// substitution the Claude Code hook makes. pi's `tool_call` block with a reason behaves like a Claude
// Code PreToolUse block, so the decision is delegated to `ast-bro hook --protocol claude-code`.
import { spawn } from "node:child_process";
import { homedir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { type ExtensionAPI, isToolCallEventType } from "@earendil-works/pi-coding-agent";

const HOOK_ARGS: string[] = __AST_BRO_HOOK_ARGS__;
const WINDOWS = process.platform === "win32";

function hook(event: object, cwd: string, signal?: AbortSignal): Promise<{ decision?: string; reason?: string } | null> {
	return new Promise((done) => {
		if (signal?.aborted) return done(null);
		let out = "";
		// npm installs `ast-bro` on Windows as a `.cmd` shim, which only cmd.exe can start; cmd also finds the
		// `.exe` of the zip and pip installs. Elsewhere the hook gets its own process group, so a timeout can
		// stop the npm wrapper and the binary it starts together.
		const child = WINDOWS
			? spawn("cmd.exe", ["/d", "/c", "ast-bro", ...HOOK_ARGS], { cwd, stdio: ["pipe", "pipe", "ignore"], windowsHide: true })
			: spawn("ast-bro", HOOK_ARGS, { cwd, stdio: ["pipe", "pipe", "ignore"], detached: true });
		const stop = () => {
			// a stuck hook, or an aborted read: stop the whole tree and let the read go ahead
			try {
				if (WINDOWS && child.pid !== undefined) {
					spawn("taskkill", ["/pid", String(child.pid), "/T", "/F"], { stdio: "ignore", windowsHide: true }).on("error", () => {});
				} else if (child.pid !== undefined) {
					process.kill(-child.pid);
				}
			} catch {}
			done(null);
		};
		const timer = setTimeout(stop, 10_000);
		signal?.addEventListener("abort", stop, { once: true });
		child.stdout.setEncoding("utf8"); // decodes a character split across two chunks
		child.stdout.on("data", (chunk) => (out += chunk));
		child.on("error", () => done(null)); // not on PATH: the read goes ahead
		child.on("close", (code) => {
			clearTimeout(timer);
			signal?.removeEventListener("abort", stop);
			try {
				done(code === 0 ? JSON.parse(out) : null);
			} catch {
				done(null);
			}
		});
		child.stdin.on("error", () => done(null)); // the hook exited before reading its input
		child.stdin.end(JSON.stringify(event));
	});
}

// The file pi's `read` opens: its `normalizePath` with `stripAtPrefix` and `normalizeUnicodeSpaces`.
function resolveLikePi(path: string, cwd: string): string {
	let p = path.replace(/[  -   　]/g, " ");
	if (p.startsWith("@")) p = p.slice(1);
	if (WINDOWS && p.startsWith("/") && !p.startsWith("//") && !p.includes("\\")) {
		const drive = p.match(/^\/(?:mnt\/|cygdrive\/)?([a-z])(?:\/(.*))?$/i);
		if (drive) p = `${drive[1].toUpperCase()}:\\${drive[2]?.replaceAll("/", "\\") ?? ""}`;
	}
	if (p === "~") p = homedir();
	else if (p.startsWith("~/") || (WINDOWS && p.startsWith("~\\"))) p = join(homedir(), p.slice(2));
	else if (p.startsWith("file://")) p = fileURLToPath(p);
	return isAbsolute(p) ? resolve(p) : resolve(cwd, p);
}

export default function (pi: ExtensionAPI) {
	pi.on("tool_call", async (event, ctx) => {
		if (!isToolCallEventType("read", event)) return;
		const { path, offset, limit } = event.input;
		if (offset !== undefined || limit !== undefined) return; // a ranged read is deliberate
		const file = resolveLikePi(path, ctx.cwd);
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
