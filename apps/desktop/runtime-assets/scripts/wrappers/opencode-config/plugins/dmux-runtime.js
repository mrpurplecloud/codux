import fs from "node:fs"

const runtimeEventDir = process.env.DMUX_RUNTIME_EVENT_DIR ?? ""
const logFile = process.env.DMUX_LOG_FILE ?? ""
const opencodeSessionMapDir = process.env.DMUX_OPENCODE_SESSION_MAP_DIR ?? ""
const runtimeSessionID = process.env.DMUX_SESSION_ID ?? ""
const runtimeTool = process.env.DMUX_ACTIVE_AI_TOOL || "opencode"

function log(message, extra) {
  if (!logFile) return
  const suffix = extra ? ` ${JSON.stringify(extra)}` : ""
  fs.appendFileSync(
    logFile,
    `[${new Date().toISOString()}] [opencode-plugin] ${message}${suffix}\n`,
  )
}

function nowSeconds() {
  return Date.now() / 1000
}

function emitTerminalProgressOsc(code) {
  if (!runtimeSessionID) return
  try {
    process.stdout.write(`\x1b]9;4;${code}\x07`)
  } catch (error) {
    log("terminal-progress-osc-error", { message: error instanceof Error ? error.message : String(error), code })
  }
}

function readManagedPrompt() {
  const path = process.env.DMUX_AI_MEMORY_PROMPT_FILE
  if (!path || typeof path !== "string") return null
  try {
    const text = fs.readFileSync(path, "utf8").trim()
    return text.length ? text : null
  } catch (error) {
    log("prompt-read-error", { message: error instanceof Error ? error.message : String(error), path })
    return null
  }
}

function sessionMapPath() {
  if (!opencodeSessionMapDir || !runtimeSessionID) return null
  return `${opencodeSessionMapDir}/opencode-session-${runtimeSessionID}.json`
}

function writeSessionMap({ externalSessionID, model }) {
  const path = sessionMapPath()
  if (!path || !externalSessionID) return
  try {
    fs.mkdirSync(opencodeSessionMapDir, { recursive: true })
    fs.writeFileSync(
      path,
      JSON.stringify(
        {
          runtimeSessionID,
          externalSessionID,
          model: model ?? null,
          updatedAt: nowSeconds(),
        },
        null,
        2,
      ),
      "utf8",
    )
  } catch (error) {
    log("session-map-write-error", { message: error instanceof Error ? error.message : String(error), path })
  }
}

function clearSessionMap(sessionID) {
  const path = sessionMapPath()
  if (!path) return
  if (sessionID && currentSessionID && sessionID !== currentSessionID) return
  try {
    fs.rmSync(path, { force: true })
  } catch (error) {
    log("session-map-clear-error", { message: error instanceof Error ? error.message : String(error), path })
  }
}

function sendRuntimePayload(payload) {
  writeRuntimeEventFile({ kind: "opencode-runtime", payload })
}

function sendAIHookPayload(payload) {
  writeRuntimeEventFile({ kind: "ai-hook", payload })
}

function writeRuntimeEventFile(envelope) {
  if (!runtimeEventDir) {
    log("event-file-skip", { reason: "no-runtime-event-dir" })
    return
  }
  try {
    fs.mkdirSync(runtimeEventDir, { recursive: true })
    const file = `${Date.now()}-${Math.random().toString(16).slice(2)}.json`
    fs.writeFileSync(`${runtimeEventDir}/${file}`, JSON.stringify(envelope), "utf8")
  } catch (error) {
    log("event-file-error", { message: error instanceof Error ? error.message : String(error), runtimeEventDir })
  }
}

function basePayload({ externalSessionID, responseState, model, status }) {
  const phase = status ?? (responseState === "idle" ? "idle" : "running")
  const currentTime = nowSeconds()
  return {
    sessionId: process.env.DMUX_SESSION_ID,
    sessionInstanceId: process.env.DMUX_SESSION_INSTANCE_ID ?? null,
    invocationId: process.env.DMUX_ACTIVE_AI_INVOCATION_ID ?? null,
    externalSessionID: externalSessionID ?? null,
    projectId: process.env.DMUX_PROJECT_ID,
    projectName: process.env.DMUX_PROJECT_NAME ?? "Workspace",
    projectPath: process.env.DMUX_PROJECT_PATH ?? "",
    sessionTitle: process.env.DMUX_SESSION_TITLE ?? "Terminal",
    tool: runtimeTool,
    model: model ?? null,
    status: phase,
    responseState: responseState ?? null,
    updatedAt: currentTime,
    startedAt: activePromptStartedAt ?? Number(process.env.DMUX_ACTIVE_AI_STARTED_AT ?? currentTime),
    finishedAt: null,
    inputTokens: lastUsage?.input ?? 0,
    outputTokens: lastUsage?.output ?? 0,
    totalTokens: lastUsage?.total ?? 0,
    contextWindow: null,
    contextUsedTokens: null,
    contextUsagePercent: null,
  }
}

function readString(value) {
  return typeof value === "string" && value.trim() ? value.trim() : null
}

function readObject(value) {
  return value && typeof value === "object" ? value : null
}

function pickFirstString(input, keys) {
  if (!input || typeof input !== "object") return null
  for (const key of keys) {
    const direct = readString(input[key])
    if (direct) return direct
  }
  for (const value of Object.values(input)) {
    const nested = readObject(value)
    if (!nested) continue
    const found = pickFirstString(nested, keys)
    if (found) return found
  }
  return null
}

function pickDirectString(input, keys) {
  if (!input || typeof input !== "object") return null
  for (const key of keys) {
    const direct = readString(input[key])
    if (direct) return direct
  }
  return null
}

function sessionIDFrom(properties) {
  const direct = pickDirectString(properties, ["sessionID", "sessionId"])
    ?? pickDirectString(readObject(properties.session), ["id", "sessionID", "sessionId"])
  if (direct) return direct
  // Only trust a bare `id` when it looks like a session id: in the OpenCode V2
  // event envelope `id` is the event id (`evt_...`), not the session.
  const fallback = pickDirectString(properties, ["id"])
  return fallback && fallback.startsWith("ses") ? fallback : null
}

// OpenCode V1 nests event fields under `properties`; V2 puts them on the event
// itself (`event.data` plus envelope fields). Normalize both to one object.
function eventFields(event) {
  const properties = readObject(event?.properties)
  if (properties) return properties
  const data = readObject(event?.data)
  if (data) return { ...event, ...data }
  return readObject(event) ?? {}
}

function messageRoleFrom(properties) {
  return pickDirectString(properties, ["role"])
    ?? pickDirectString(readObject(properties.message), ["role"])
}

function modelFrom(properties) {
  return pickDirectString(properties, ["modelID", "modelId", "model"])
    ?? pickDirectString(readObject(properties.model), ["id", "modelID", "modelId", "model"])
    ?? pickDirectString(readObject(properties.message), ["modelID", "modelId", "model"])
}

function messageIDFrom(properties) {
  return pickDirectString(properties, ["messageID", "messageId", "id"])
    ?? pickDirectString(readObject(properties.message), ["id", "messageID", "messageId"])
}

function commandNameFrom(properties) {
  return pickDirectString(properties, ["command", "name"])
}

function messageFrom(properties) {
  return pickDirectString(properties, ["message", "prompt", "description"])
}

function toolNameFrom(properties) {
  return pickDirectString(properties, ["toolName", "tool", "name"])
}

let currentSessionID = readString(process.env.DMUX_EXTERNAL_SESSION_ID)
let currentModel = null
let lastUserMessageID = null
let lastSignature = null
let hasActivePrompt = false
let activePromptStartedAt = null
let lastUsage = null

function dispatchUpdate({ externalSessionID, responseState, model, status, reason }) {
  const nextExternalSessionID = readString(externalSessionID) ?? currentSessionID
  if (!nextExternalSessionID) {
    log("skip-dispatch-missing-session", { reason })
    return
  }
  currentSessionID = nextExternalSessionID
  currentModel = readString(model) ?? currentModel
  writeSessionMap({ externalSessionID: currentSessionID, model: currentModel })
  const signature = `${nextExternalSessionID}|${responseState ?? "nil"}|${currentModel ?? "nil"}`
  if (signature == lastSignature) {
    return
  }
  lastSignature = signature
  sendRuntimePayload(
    basePayload({
      externalSessionID: nextExternalSessionID,
      responseState,
      model: currentModel,
      status,
    }),
  )
  log("dispatch", { reason, externalSessionID: nextExternalSessionID, responseState: responseState ?? null, model: currentModel })
}

function dispatchAIHook({
  kind,
  externalSessionID,
  model,
  totalTokens,
  notificationType,
  reason,
  targetToolName,
  message,
}) {
  const nextExternalSessionID = readString(externalSessionID) ?? currentSessionID
  const nextModel = readString(model) ?? currentModel
  sendAIHookPayload({
    kind,
    terminalID: process.env.DMUX_SESSION_ID,
    terminalInstanceID: process.env.DMUX_SESSION_INSTANCE_ID ?? null,
    projectID: process.env.DMUX_PROJECT_ID,
    projectName: process.env.DMUX_PROJECT_NAME ?? "Workspace",
    projectPath: process.env.DMUX_PROJECT_PATH ?? "",
    sessionTitle: process.env.DMUX_SESSION_TITLE ?? "Terminal",
    tool: runtimeTool,
    aiSessionID: nextExternalSessionID ?? null,
    model: nextModel ?? null,
    totalTokens: Number.isInteger(totalTokens) ? totalTokens : null,
    updatedAt: nowSeconds(),
    metadata: {
      transcriptPath: null,
      notificationType: notificationType ?? null,
      source: null,
      reason: reason ?? null,
      targetToolName: targetToolName ?? null,
      message: message ?? null,
    },
  })
}

function initRuntime() {
  log("initialized", {
    hasEventDir: Boolean(runtimeEventDir),
    sessionID: process.env.DMUX_SESSION_ID ?? null,
    externalSessionID: currentSessionID,
  })
  if (currentSessionID) {
    writeSessionMap({ externalSessionID: currentSessionID, model: currentModel })
  }
}

async function handleEvent(event) {
  const type = event?.type
  const properties = eventFields(event)

  switch (type) {
        case "session.created": {
          dispatchAIHook({
            kind: "sessionStarted",
            externalSessionID: sessionIDFrom(properties),
            model: modelFrom(properties),
            totalTokens: null,
            reason: type,
          })
          dispatchUpdate({
            externalSessionID: sessionIDFrom(properties),
            responseState: null,
            model: modelFrom(properties),
            reason: type,
          })
          return
        }
        case "session.updated": {
          const sessionID = sessionIDFrom(properties)
          const model = modelFrom(properties)
          if (sessionID) {
            currentSessionID = sessionID
            currentModel = model ?? currentModel
            writeSessionMap({ externalSessionID: currentSessionID, model: currentModel })
          }
          log("session-updated", { externalSessionID: sessionID ?? null, model: model ?? null })
          return
        }
        case "session.status": {
          const sessionID = sessionIDFrom(properties)
          const status = readObject(properties.status)
          const statusType = readString(status?.type)
          if (statusType === "busy" || statusType === "retry") {
            hasActivePrompt = true
            activePromptStartedAt = activePromptStartedAt ?? nowSeconds()
            emitTerminalProgressOsc("3")
            dispatchUpdate({
              externalSessionID: sessionID,
              responseState: "responding",
              model: modelFrom(properties),
              reason: `status:${statusType}`,
            })
            return
          }
          if (statusType === "idle") {
            if (hasActivePrompt) {
              emitTerminalProgressOsc("0")
            }
            dispatchUpdate({
              externalSessionID: sessionID,
              responseState: "idle",
              model: modelFrom(properties),
              status: "idle",
              reason: `status:${statusType}`,
            })
            log("status-idle", { externalSessionID: sessionID ?? null })
          }
          return
        }
        case "session.idle": {
          const sessionID = sessionIDFrom(properties)
          if (!hasActivePrompt) {
            log("ignore-session-idle", { externalSessionID: sessionID ?? null, reason: "no-active-prompt" })
            return
          }
          hasActivePrompt = false
          activePromptStartedAt = null
          emitTerminalProgressOsc("0")
          dispatchAIHook({
            kind: "turnCompleted",
            externalSessionID: sessionID,
            model: modelFrom(properties),
            totalTokens: null,
            reason: type,
          })
          dispatchUpdate({
            externalSessionID: sessionID,
            responseState: "idle",
            model: modelFrom(properties),
            status: "completed",
            reason: type,
          })
          return
        }
        case "session.deleted": {
          dispatchAIHook({
            kind: "sessionEnded",
            externalSessionID: sessionIDFrom(properties),
            model: modelFrom(properties),
            totalTokens: null,
            reason: type,
          })
          clearSessionMap(sessionIDFrom(properties))
          return
        }
        case "message.updated": {
          const role = messageRoleFrom(properties)
          const model = modelFrom(properties)
          const sessionID = sessionIDFrom(properties)
          if (role === "user") {
            const messageID = messageIDFrom(properties) ?? `user-${Date.now()}`
            if (messageID === lastUserMessageID) return
            lastUserMessageID = messageID
            hasActivePrompt = true
            activePromptStartedAt = nowSeconds()
            emitTerminalProgressOsc("3")
            dispatchUpdate({
              externalSessionID: sessionID,
              responseState: "responding",
              model,
              reason: "message:user",
            })
            dispatchAIHook({
              kind: "promptSubmitted",
              externalSessionID: sessionID,
              model,
              totalTokens: null,
              reason: "message:user",
            })
            return
          }
          if (role) {
            log("ignore-message-updated", { role, externalSessionID: sessionID ?? null })
          }
          if (model) {
            currentModel = model
          }
          return
        }
        case "tui.command.execute": {
          const commandName = commandNameFrom(properties)
          if (commandName === "sessions" || commandName === "resume" || commandName === "continue") {
            dispatchAIHook({
              kind: "sessionStarted",
              externalSessionID: sessionIDFrom(properties),
              model: modelFrom(properties),
              totalTokens: null,
              reason: `command:${commandName}`,
            })
            dispatchUpdate({
              externalSessionID: sessionIDFrom(properties),
              responseState: null,
              model: modelFrom(properties),
              reason: `command:${commandName}`,
            })
          }
          return
        }
        case "permission.asked": {
          const resources = Array.isArray(properties.resources)
            ? properties.resources.map((value) => readString(value)).filter(Boolean)
            : []
          dispatchAIHook({
            kind: "needsInput",
            externalSessionID: sessionIDFrom(properties),
            model: modelFrom(properties),
            totalTokens: null,
            notificationType: "permission-request",
            reason: type,
            targetToolName: toolNameFrom(properties) ?? readString(properties.action),
            message: messageFrom(properties) ?? (resources.length ? resources.join(", ") : null),
          })
          return
        }
        case "permission.replied": {
          dispatchAIHook({
            kind: "promptSubmitted",
            externalSessionID: sessionIDFrom(properties),
            model: modelFrom(properties),
            totalTokens: null,
            reason: type,
          })
          return
        }
        // ---- OpenCode V2 event names ----
        case "session.execution.started": {
          hasActivePrompt = true
          activePromptStartedAt = activePromptStartedAt ?? nowSeconds()
          emitTerminalProgressOsc("3")
          dispatchUpdate({
            externalSessionID: sessionIDFrom(properties),
            responseState: "responding",
            model: modelFrom(properties),
            reason: type,
          })
          return
        }
        case "session.execution.succeeded":
        case "session.execution.failed":
        case "session.execution.interrupted": {
          const sessionID = sessionIDFrom(properties)
          if (hasActivePrompt) {
            emitTerminalProgressOsc("0")
          }
          hasActivePrompt = false
          activePromptStartedAt = null
          dispatchAIHook({
            kind: "turnCompleted",
            externalSessionID: sessionID,
            model: modelFrom(properties),
            totalTokens: lastUsage?.total ?? null,
            reason: type,
          })
          dispatchUpdate({
            externalSessionID: sessionID,
            responseState: "idle",
            model: modelFrom(properties),
            status: "completed",
            reason: type,
          })
          return
        }
        case "session.step.started": {
          const model = modelFrom(properties)
          if (model && model !== currentModel) {
            currentModel = model
            dispatchUpdate({
              externalSessionID: sessionIDFrom(properties),
              responseState: "responding",
              model,
              reason: type,
            })
          }
          return
        }
        case "session.inbox.enqueued": {
          const item = readObject(properties.item)
          if (!item || item.type !== "user") return
          const sessionID = sessionIDFrom(properties)
          const text = readString(readObject(item.payload)?.text)
          const messageID = readString(properties.inboxID) ?? `user-${Date.now()}`
          if (messageID === lastUserMessageID) return
          lastUserMessageID = messageID
          hasActivePrompt = true
          activePromptStartedAt = nowSeconds()
          emitTerminalProgressOsc("3")
          dispatchUpdate({
            externalSessionID: sessionID,
            responseState: "responding",
            model: modelFrom(properties),
            reason: type,
          })
          dispatchAIHook({
            kind: "promptSubmitted",
            externalSessionID: sessionID,
            model: modelFrom(properties),
            totalTokens: null,
            reason: type,
            message: text,
          })
          return
        }
        case "session.usage.updated": {
          const tokens = readObject(properties.tokens)
          if (!tokens) return
          const input = Number.isInteger(tokens.input) ? tokens.input : 0
          const output = Number.isInteger(tokens.output) ? tokens.output : 0
          lastUsage = { input, output, total: input + output }
          return
        }
        case "session.renamed": {
          const title = readString(properties.title)
          if (title) {
            log("session-renamed", { externalSessionID: sessionIDFrom(properties) ?? null, title })
          }
          return
        }
        default:
          return
      }
}

function injectManagedPrompt(output) {
  const prompt = readManagedPrompt()
  if (!prompt) {
    log("system-transform-skip", { reason: "missing-prompt" })
    return
  }
  if (!Array.isArray(output.system)) {
    output.system = []
  }
  if (output.system.includes(prompt)) {
    return
  }
  output.system.unshift(prompt)
  log("system-transform-injected", { chars: prompt.length })
}

// OpenCode V1 entrypoint (kept for older loaders).
export const DmuxRuntimePlugin = async () => {
  initRuntime()

  return {
    "experimental.chat.system.transform": async (_input, output) => {
      injectManagedPrompt(output)
    },

    event: async ({ event }) => {
      await handleEvent(event)
    },
  }
}

// OpenCode V2 entrypoint: a default export with an id and a setup function.
export default {
  id: "dmux-runtime",
  async setup(ctx) {
    initRuntime()

    if (ctx?.session && typeof ctx.session.hook === "function") {
      await ctx.session.hook("context", (event) => {
        const prompt = readManagedPrompt()
        if (!prompt) {
          log("system-transform-skip", { reason: "missing-prompt" })
          return
        }
        if (!Array.isArray(event.system)) {
          event.system = []
        }
        const already = event.system.some(
          (part) => part && typeof part === "object" && part.type === "text" && part.text === prompt,
        )
        if (already || event.system.includes(prompt)) {
          return
        }
        event.system.unshift({ type: "text", text: prompt })
        log("system-transform-injected", { chars: prompt.length })
      })
    } else {
      log("context-hook-unavailable")
    }

    const controller = new AbortController()
    if (ctx?.event && typeof ctx.event.subscribe === "function") {
      void (async () => {
        try {
          for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
            await handleEvent(event)
          }
        } catch (error) {
          log("event-stream-error", { message: error instanceof Error ? error.message : String(error) })
        }
      })()
    } else {
      log("event-subscribe-unavailable")
    }

    return () => controller.abort()
  },
}
