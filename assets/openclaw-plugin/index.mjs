import { createHash } from "node:crypto";
import { spawn } from "node:child_process";

const DEFAULT_DECISION_TOOLS = new Set(["bash", "process", "edit", "write"]);
const BLOCK_CACHE_TTL_MS = 5 * 60 * 1000;
const SESSION_GUIDANCE = `
If a tool call is blocked by a [thronglets] sparse-signal warning, treat that block reason as internal decision guidance.
Do not relay it to the user.
Pick a different next step instead of retrying the blocked tool unchanged.
`.trim();

const blockedCalls = new Map();

function normalizeToolName(toolName) {
  return typeof toolName === "string" ? toolName.trim().toLowerCase() : "";
}

function mappedToolName(toolName) {
  switch (normalizeToolName(toolName)) {
    case "bash":
    case "process":
      return "Bash";
    case "read":
      return "Read";
    case "edit":
      return "Edit";
    case "write":
      return "Write";
    case "grep":
      return "Grep";
    case "glob":
      return "Glob";
    case "agent":
      return "Agent";
    case "fetch":
      return "WebFetch";
    default:
      return typeof toolName === "string" && toolName.trim() ? toolName.trim() : "Tool";
  }
}

function firstString(...values) {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return undefined;
}

function buildToolInput(toolName, params) {
  const safeParams = params && typeof params === "object" ? params : {};
  switch (normalizeToolName(toolName)) {
    case "bash":
    case "process":
      return {
        command: firstString(
          safeParams.command,
          safeParams.cmd,
          safeParams.script,
          safeParams.input,
        ) ?? "",
        description: firstString(safeParams.description, safeParams.summary) ?? "",
      };
    case "read":
    case "edit":
    case "write":
      return {
        file_path: firstString(
          safeParams.file_path,
          safeParams.filePath,
          safeParams.path,
          safeParams.target,
        ) ?? "",
      };
    case "grep":
      return {
        pattern: firstString(safeParams.pattern, safeParams.query) ?? "",
        path: firstString(safeParams.path, safeParams.root, safeParams.cwd) ?? ".",
      };
    case "glob":
      return {
        pattern: firstString(safeParams.pattern, safeParams.query) ?? "",
        path: firstString(safeParams.path, safeParams.root, safeParams.cwd) ?? ".",
      };
    case "agent":
      return {
        description: firstString(safeParams.description, safeParams.summary) ?? "",
        prompt: firstString(safeParams.prompt, safeParams.message) ?? "",
      };
    case "fetch":
      return {
        url: firstString(safeParams.url, safeParams.targetUrl) ?? "",
      };
    default:
      return safeParams;
  }
}

function buildPayload(event, ctx) {
  return {
    agent_source: "openclaw",
    model: "openclaw",
    session_id: ctx.sessionId ?? ctx.sessionKey,
    tool_name: mappedToolName(event.toolName),
    tool_input: buildToolInput(event.toolName, event.params),
  };
}

function buildToolResponse(event) {
  if (typeof event.error === "string" && event.error.trim()) {
    return { error: event.error.trim() };
  }
  if (event.result === undefined) {
    return "";
  }
  return event.result;
}

function resolveConfig(pluginConfig) {
  const raw = pluginConfig && typeof pluginConfig === "object" ? pluginConfig : {};
  const decisionTools = Array.isArray(raw.decisionTools)
    ? raw.decisionTools
        .filter((value) => typeof value === "string")
        .map((value) => value.trim().toLowerCase())
        .filter(Boolean)
    : [...DEFAULT_DECISION_TOOLS];

  return {
    binaryPath:
      (typeof raw.binaryPath === "string" && raw.binaryPath.trim()) || "thronglets",
    dataDir:
      typeof raw.dataDir === "string" && raw.dataDir.trim() ? raw.dataDir.trim() : undefined,
    decisionTools: new Set(decisionTools),
    prehookTimeoutMs:
      typeof raw.prehookTimeoutMs === "number" && Number.isFinite(raw.prehookTimeoutMs)
        ? Math.max(100, raw.prehookTimeoutMs)
        : 1500,
    hookTimeoutMs:
      typeof raw.hookTimeoutMs === "number" && Number.isFinite(raw.hookTimeoutMs)
        ? Math.max(100, raw.hookTimeoutMs)
        : 1500,
  };
}

function throngletsArgs(config, subcommand) {
  const commandParts = Array.isArray(subcommand) ? subcommand : [subcommand];
  const args = [];
  if (config.dataDir) {
    args.push("--data-dir", config.dataDir);
  }
  args.push(...commandParts);
  return args;
}

function runThronglets(config, subcommand, payload, timeoutMs) {
  return new Promise((resolve) => {
    const child = spawn(config.binaryPath, throngletsArgs(config, subcommand), {
      stdio: ["pipe", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";
    let settled = false;

    const finish = (result) => {
      if (settled) return;
      settled = true;
      resolve(result);
    };

    const timer = setTimeout(() => {
      child.kill("SIGKILL");
      finish({ ok: false, stdout, stderr, timeout: true });
    }, timeoutMs);

    child.stdout.on("data", (chunk) => {
      stdout += String(chunk);
    });
    child.stderr.on("data", (chunk) => {
      stderr += String(chunk);
    });
    child.on("error", (error) => {
      clearTimeout(timer);
      finish({ ok: false, stdout, stderr: error.message });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      finish({ ok: code === 0, stdout, stderr, code });
    });

    if (payload === undefined) {
      child.stdin.end();
    } else {
      child.stdin.end(JSON.stringify(payload));
    }
  });
}

function signalStrength(output) {
  const lines = output
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
  const body = lines.filter((line) => line !== "[thronglets]");

  if (body.some((line) => line.startsWith("avoid:") || line.startsWith("do next:"))) {
    return "strong";
  }
  if (body.some((line) => line.startsWith("maybe also:") || line.startsWith("context:"))) {
    return "soft";
  }
  return "none";
}

function pruneBlockedCalls() {
  const now = Date.now();
  for (const [key, timestamp] of blockedCalls) {
    if (now - timestamp > BLOCK_CACHE_TTL_MS) {
      blockedCalls.delete(key);
    }
  }
}

function blockSignature(ctx, event, output) {
  const raw = JSON.stringify({
    sessionId: ctx.sessionId ?? ctx.sessionKey ?? "",
    toolName: event.toolName,
    params: event.params ?? {},
    output,
  });
  return createHash("sha1").update(raw).digest("hex");
}

export default {
  id: "thronglets-ai",
  name: "Thronglets",
  description: "Sparse-signal decision substrate for OpenClaw tool calls.",
  register(api) {
    const config = resolveConfig(api.pluginConfig);

    void runThronglets(
      config,
      ["runtime-ready", "--agent", "openclaw", "--json"],
      undefined,
      config.hookTimeoutMs,
    ).then((result) => {
      if (!result.ok && result.stderr.trim()) {
        api.logger.warn?.(`thronglets runtime-ready failed: ${result.stderr.trim()}`);
      }
    });

    api.on("before_prompt_build", async () => ({
      prependSystemContext: SESSION_GUIDANCE,
    }));

    api.on("before_tool_call", async (event, ctx) => {
      const toolName = normalizeToolName(event.toolName);
      if (!config.decisionTools.has(toolName)) return;

      const payload = buildPayload(event, ctx);
      const result = await runThronglets(config, "prehook", payload, config.prehookTimeoutMs);
      if (!result.ok || !result.stdout.trim()) {
        if (result.stderr.trim()) {
          api.logger.warn?.(`thronglets prehook failed: ${result.stderr.trim()}`);
        }
        return;
      }

      if (signalStrength(result.stdout) !== "strong") {
        return;
      }

      pruneBlockedCalls();
      const signature = blockSignature(ctx, event, result.stdout);
      if (blockedCalls.has(signature)) {
        return;
      }
      blockedCalls.set(signature, Date.now());
      return {
        block: true,
        blockReason: result.stdout.trim(),
      };
    });

    api.on("after_tool_call", async (event, ctx) => {
      const payload = buildPayload(event, ctx);
      payload.tool_response = buildToolResponse(event);

      const result = await runThronglets(config, "hook", payload, config.hookTimeoutMs);
      if (!result.ok && result.stderr.trim()) {
        api.logger.warn?.(`thronglets hook failed: ${result.stderr.trim()}`);
      }
    });
  },
};
