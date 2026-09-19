interface DefaultModel {
  id: string;
  name: string;
  alias: string;
  envKey?: string;
  defaultValue?: string;
  isTopLevel?: boolean;
}

interface Note {
  type: "info" | "warning" | "cloudCheck" | "error";
  text: string;
}

interface GuideStep {
  step: number;
  title: string;
  desc?: string;
  value?: string;
  copyable?: boolean;
  type?: "apiKeySelector" | "modelSelector";
}

interface CodeBlock {
  language: string;
  code: string;
}

interface EnvVars {
  baseUrl: string;
  model: string;
  opusModel?: string;
  sonnetModel?: string;
  fableModel?: string;
  haikuModel?: string;
}

interface CLITool {
  id: string;
  name: string;
  icon?: string;
  image?: string;
  color: string;
  description: string;
  configType: "env" | "custom" | "guide";
  envVars?: EnvVars;
  modelAliases?: string[];
  settingsFile?: string;
  defaultModels?: DefaultModel[];
  requiresExternalUrl?: boolean;
  docsUrl?: string;
  defaultCommand?: string;
  notes?: Note[];
  guideSteps?: GuideStep[];
  codeBlock?: CodeBlock;
}

export const CLI_TOOLS: Record<string, CLITool> = {
  claude: {
    id: "claude",
    name: "Claude Code",
    image: "/providers/claude.png",
    color: "#D97757",
    description: "Anthropic Claude Code CLI",
    configType: "env",
    envVars: {
      baseUrl: "ANTHROPIC_BASE_URL",
      model: "ANTHROPIC_MODEL",
      opusModel: "ANTHROPIC_DEFAULT_OPUS_MODEL",
      sonnetModel: "ANTHROPIC_DEFAULT_SONNET_MODEL",
      fableModel: "ANTHROPIC_DEFAULT_FABLE_MODEL",
      haikuModel: "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    },
    modelAliases: ["default", "sonnet", "opus", "fable", "haiku", "opusplan"],
    settingsFile: "~/.claude/settings.json",
    defaultModels: [
      { id: "fable", name: "Claude Fable", alias: "fable", envKey: "ANTHROPIC_DEFAULT_FABLE_MODEL", defaultValue: "cc/claude-fable-5" },
      { id: "opus", name: "Claude Opus", alias: "opus", envKey: "ANTHROPIC_DEFAULT_OPUS_MODEL", defaultValue: "cc/claude-opus-5" },
      { id: "sonnet", name: "Claude Sonnet", alias: "sonnet", envKey: "ANTHROPIC_DEFAULT_SONNET_MODEL", defaultValue: "cc/claude-sonnet-5" },
      { id: "haiku", name: "Claude Haiku", alias: "haiku", envKey: "ANTHROPIC_DEFAULT_HAIKU_MODEL", defaultValue: "cc/claude-haiku-4-5-20251001" },
    ],
  },
  codex: {
    id: "codex",
    name: "OpenAI Codex CLI / App",
    image: "/providers/codex.png",
    color: "#10A37F",
    description: "OpenAI Codex CLI",
    configType: "custom",
  },
  opencode: {
    id: "opencode",
    name: "OpenCode",
    image: "/providers/opencode.png",
    color: "#E87040",
    description: "OpenCode AI Terminal Assistant",
    configType: "custom",
  },
  cline: {
    id: "cline",
    name: "Cline",
    image: "/providers/cline.png",
    color: "#00D1B2",
    description: "Cline AI Coding Assistant",
    configType: "custom",
    guideSteps: [
      { step: 1, title: "Open Settings", desc: "Go to Cline Settings panel" },
      { step: 2, title: "Select Provider", desc: "Choose API Provider → OpenAI Compatible" },
      { step: 3, title: "Base URL", value: "{{baseUrl}}/v1", copyable: true },
      { step: 4, title: "API Key", type: "apiKeySelector" },
      { step: 5, title: "Select Model", type: "modelSelector" },
    ],
  },
  roo: {
    id: "roo",
    name: "Roo",
    image: "/providers/roo.png",
    color: "#FF6B6B",
    description: "Roo AI Assistant",
    configType: "guide",
    guideSteps: [
      { step: 1, title: "Open Settings", desc: "Go to Roo Settings panel" },
      { step: 2, title: "Select Provider", desc: "Choose API Provider → Ollama" },
      { step: 3, title: "Base URL", value: "{{baseUrl}}", copyable: true },
      { step: 4, title: "API Key", type: "apiKeySelector" },
      { step: 5, title: "Select Model", type: "modelSelector" },
    ],
  },
  continue: {
    id: "continue",
    name: "Continue",
    image: "/providers/continue.png",
    color: "#7C3AED",
    description: "Continue AI Assistant",
    configType: "guide",
    guideSteps: [
      { step: 1, title: "Open Config", desc: "Open Continue configuration file" },
      { step: 2, title: "API Key", type: "apiKeySelector" },
      { step: 3, title: "Select Model", type: "modelSelector" },
      { step: 4, title: "Add Model Config", desc: "Add the following configuration to your models array:" },
    ],
    codeBlock: {
      language: "json",
      code: `{
  "apiBase": "{{baseUrl}}",
  "title": "{{model}}",
  "model": "{{model}}",
  "provider": "openai",
  "apiKey": "{{apiKey}}"
}`,
    },
  },
  amp: {
    id: "amp",
    name: "Amp CLI",
    image: "/providers/amp.png",
    color: "#F97316",
    description: "Sourcegraph Amp coding assistant CLI",
    docsUrl: "/docs?section=cli-tools&tool=amp",
    configType: "guide",
    defaultCommand: "amp",
    modelAliases: ["g25p", "g25f", "cs45", "g54"],
    notes: [
      { type: "info", text: "Use OpenProxy model aliases to keep Amp shorthand mappings stable across provider updates." },
      { type: "warning", text: "Suggested shorthand examples: g25p → gemini/gemini-2.5-pro, g25f → gemini/gemini-2.5-flash, cs45 → cc/claude-sonnet-4-5-20250929." },
    ],
    guideSteps: [
      { step: 1, title: "Install Amp", desc: "Install the Amp CLI using the package manager supported by your environment." },
      { step: 2, title: "API Key", type: "apiKeySelector" },
      { step: 3, title: "Base URL", value: "{{baseUrl}}", copyable: true },
      { step: 4, title: "Select Model", type: "modelSelector" },
      { step: 5, title: "Add Shorthands", desc: "Map Amp shorthand names such as g25p or cs45 to OpenProxy aliases in your local config." },
    ],
    codeBlock: {
      language: "bash",
      code: `export OPENAI_API_KEY="{{apiKey}}"
export OPENAI_BASE_URL="{{baseUrl}}"
amp --model "{{model}}"
# Example shorthand aliases you can map locally:
# g25p -> gemini/gemini-2.5-pro
# cs45 -> cc/claude-sonnet-4-5-20250929`,
    },
  },
  "deepseek-tui": {
    id: "deepseek-tui",
    name: "DeepSeek TUI",
    image: "/providers/deepseek-tui.png",
    color: "#4D6BFE",
    description: "DeepSeek Terminal Coding Agent (Rust TUI)",
    docsUrl: "https://github.com/DeepSeek-TUI/DeepSeek-TUI",
    configType: "custom",
    defaultCommand: "deepseek",
    modelAliases: ["deepseek-v4-pro", "deepseek-v4-flash", "deepseek-chat", "deepseek-reasoner"],
    defaultModels: [
      { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro", alias: "deepseek-v4-pro" },
      { id: "deepseek-v4-flash", name: "DeepSeek V4 Flash", alias: "deepseek-v4-flash" },
      { id: "deepseek-chat", name: "DeepSeek V3 Chat", alias: "deepseek-chat" },
    ],
    notes: [
      { type: "info", text: "DeepSeek TUI uses ~/.deepseek/config.toml for configuration. OpenProxy will update the provider to 'openai' mode with your base_url, api_key, and model." },
      { type: "warning", text: "Config path: Linux/macOS ~/.deepseek/config.toml • Windows %USERPROFILE%\\.deepseek\\config.toml" },
    ],
  },
};

interface ProviderConnection {
  id: string;
  isActive: boolean;
  testStatus: string;
  provider: string;
  name: string;
  models?: string[];
}

interface ProviderModelForMapping {
  connectionId: string;
  provider: string;
  name: string;
  models: string[];
}

export function getProviderModelsForMapping(providers: ProviderConnection[]): ProviderModelForMapping[] {
  const result: ProviderModelForMapping[] = [];
  providers.forEach(conn => {
    if (conn.isActive && (conn.testStatus === "active" || conn.testStatus === "success")) {
      result.push({
        connectionId: conn.id,
        provider: conn.provider,
        name: conn.name,
        models: conn.models || [],
      });
    }
  });
  return result;
}
