// Agent Skills metadata — single source of truth for /dashboard/skills page.
// Each skill = 1 raw GitHub URL the user copies and pastes to any AI agent.

const REPO = "quangdang46/openproxy";
const BRANCH = "main";
const SKILL_PATH = ".agents/skills";

export const SKILLS_REPO_URL = `https://github.com/${REPO}`;
export const SKILLS_RAW_BASE = `https://raw.githubusercontent.com/${REPO}/refs/heads/${BRANCH}/${SKILL_PATH}`;
export const SKILLS_BLOB_BASE = `https://github.com/${REPO}/blob/${BRANCH}/${SKILL_PATH}`;

export interface Skill {
  id: string;
  name: string;
  description: string;
  endpoint: string | null;
  icon: string;
  isEntry?: boolean;
}

export const SKILLS: Skill[] = [
  {
    id: "openproxy",
    name: "OpenProxy (Entry)",
    description: "Setup + index of all capabilities. Start here — covers install, server init, provider setup, and wiring every AI coding CLI tool.",
    endpoint: null,
    icon: "hub",
    isEntry: true,
  },
  {
    id: "openproxy-chat",
    name: "Chat",
    description: "Chat / code-gen via the OpenAI-compatible API with streaming, multi-modal support, and format translation.",
    endpoint: "/v1/chat/completions",
    icon: "chat",
  },
  {
    id: "openproxy-web-fetch",
    name: "Web Fetch",
    description: "URL-to-markdown fetch routed through proxied fetch providers.",
    endpoint: "/v1/web/fetch",
    icon: "language",
  },
  {
    id: "openproxy-providers",
    name: "Providers",
    description: "Configure AI providers: OAuth (Claude Code, Codex, Copilot), API key (OpenAI, Anthropic, Gemini — 40+), and free tiers (Vertex AI).",
    endpoint: null,
    icon: "cloud",
  },
  {
    id: "openproxy-cli-tools",
    name: "CLI Tools",
    description: "Wire Claude Code, Codex, Cline, Continue, Roo, Kilo, Copilot, OpenClaw, and more into OpenProxy with one-click configuration.",
    endpoint: null,
    icon: "terminal",
  },
  {
    id: "openproxy-rtk",
    name: "RTK Token Compression",
    description: "Reduce input tokens by 20-40% via runtime token compression of tool-call results. Lower latency and cost on every request.",
    endpoint: null,
    icon: "compress",
  },
];

export function getSkillRawUrl(id: string): string {
  return `${SKILLS_RAW_BASE}/${id}/SKILL.md`;
}

export function getSkillBlobUrl(id: string): string {
  return `${SKILLS_BLOB_BASE}/${id}/SKILL.md`;
}
