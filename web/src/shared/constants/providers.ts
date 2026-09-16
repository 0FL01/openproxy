// Provider definitions

import type {
  Provider,
  FreeTierInfo,
  ThinkingConfig,
  AuthMethod
} from "../../types";

// Free Providers (kiro first)
export const FREE_PROVIDERS: Record<string, Provider> = {
  kiro: { id: "kiro", alias: "kr", name: "Kiro AI", icon: "psychology_alt", color: "#FF6B35", website: "https://kiro.dev", notice: { signupUrl: "https://kiro.dev" } },
  qwen: { id: "qwen", alias: "qw", name: "Qwen Code", icon: "psychology", color: "#10B981", deprecated: true, deprecationNotice: "Qwen OAuth free tier was discontinued by Alibaba on 2026-04-15. New connections will not work.", website: "https://chat.qwen.ai", notice: { signupUrl: "https://chat.qwen.ai" }, serviceKinds: ["llm"] },
  // gitlab: { id: "gitlab", alias: "gl", name: "GitLab Duo", icon: "code", color: "#FC6D26" },
  // codebuddy: { id: "codebuddy", alias: "cb", name: "CodeBuddy", icon: "smart_toy", color: "#006EFF" },
  "opencode-zen": { id: "opencode-zen", alias: "opencode-zen", name: "OpenCode Zen", icon: "terminal", color: "#E87040", textIcon: "OC", noAuth: true, passthroughModels: true },
};

// Free Tier Providers (has free access but may require account/API key)
export const FREE_TIER_PROVIDERS: Record<string, Provider> = {
  openrouter: { id: "openrouter", alias: "openrouter", name: "OpenRouter", icon: "router", color: "#F97316", textIcon: "OR", website: "https://openrouter.ai", notice: { text: "Free tier: 27+ free models, no credit card needed, 200 req/day. After $10 credit: 1,000 req/day.", apiKeyUrl: "https://openrouter.ai/settings/keys" }, modelsFetcher: { url: "https://openrouter.ai/api/v1/models", type: "openrouter-free" }, passthroughModels: true, serviceKinds: ["llm", "imageToText"] },
  nvidia: { id: "nvidia", alias: "nvidia", name: "NVIDIA NIM", icon: "developer_board", color: "#76B900", textIcon: "NV", website: "https://developer.nvidia.com/nim", notice: { text: "Free access for NVIDIA Developer Program members (prototyping & testing).", apiKeyUrl: "https://build.nvidia.com/settings/api-keys" }, serviceKinds: ["llm"] },
  ollama: { id: "ollama", alias: "ollama", name: "Ollama Cloud", icon: "cloud", color: "#ffffffff", textIcon: "OL", website: "https://ollama.com", notice: { text: "Free tier: light usage, 1 cloud model at a time (limits reset every 5h & 7d). Pro $20/mo · Max $100/mo.", apiKeyUrl: "https://ollama.com/settings/keys" } },
  vertex: { id: "vertex", alias: "vx", name: "Vertex AI", icon: "cloud", color: "#4285F4", textIcon: "VX", website: "https://cloud.google.com/vertex-ai", notice: { text: "New Google Cloud accounts get $300 free credits. Requires GCP project + Service Account with Vertex AI API enabled.", apiKeyUrl: "https://console.cloud.google.com/iam-admin/serviceaccounts" } },
  gemini: { id: "gemini", alias: "gemini", name: "Gemini", icon: "diamond", color: "#4285F4", textIcon: "GE", website: "https://ai.google.dev", notice: { apiKeyUrl: "https://aistudio.google.com/app/apikey" }, serviceKinds: ["llm", "imageToText"] },
  "cloudflare-ai": { id: "cloudflare-ai", alias: "cf", name: "Cloudflare", icon: "cloud", color: "#F38020", textIcon: "CF", website: "https://developers.cloudflare.com/workers-ai/", notice: { text: "Workers AI free tier. Requires a Cloudflare API token and Account ID.", apiKeyUrl: "https://dash.cloudflare.com/profile/api-tokens" }, serviceKinds: ["llm"], hasProviderSpecificData: true },
  tokenrouter: { id: "tokenrouter", alias: "tr", name: "TokenRouter", icon: "router", color: "#EF4444", textIcon: "TR", website: "https://tokenrouter.com", notice: { text: "Free promotional models: qwen/qwen3.8-max-free, moonshotai/kimi-k3-free. Requires Bearer token authentication.", apiKeyUrl: "https://tokenrouter.com/settings/api-keys" }, serviceKinds: ["llm"] },
  // Free-tier providers whose canonical definitions live elsewhere
  // (FREE_PROVIDERS / OAUTH_PROVIDERS / APIKEY_PROVIDERS). Duplicated here so
  // the dashboard can categorize them as free tier without touching their
  // auth behavior. AI_PROVIDERS spreads OAUTH_PROVIDERS after this, so
  // kilocode's oauth+apikey authModes are preserved.
  "opencode-zen": { id: "opencode-zen", alias: "opencode-zen", name: "OpenCode Zen", icon: "terminal", color: "#E87040", textIcon: "OC", noAuth: true, passthroughModels: true },
  kilocode: { id: "kilocode", alias: "kc", name: "Kilo Code", icon: "code", color: "#FF6B35", textIcon: "KC", website: "https://kilocode.ai", notice: { signupUrl: "https://kilocode.ai", apiKeyUrl: "https://kilocode.ai" }, authModes: ["oauth", "apikey"], hasOAuth: true, priority: 40 },
};

// Single source of truth for free-tier provider IDs (categorization shared
// across backend + frontend). These are the providers the dashboard treats
// as free tier. Keep in sync with FREE_TIER_PROVIDERS above.
export const FREE_TIER_PROVIDER_IDS: string[] = [
  "nvidia",
  "opencode-zen",
  "openrouter",
  "kilocode",
  "ollama",
  "gemini",
  "modelscope",
  "aion",
  "agnes",
  "ai21",
  "ovhcloud",
  "mistral",
  "llm7",
  "sambanova",
  "kiro",
];

// O(1) membership lookup derived from the canonical ID list.
export const FREE_TIER_SET: Set<string> = new Set(FREE_TIER_PROVIDER_IDS);

// Helper: is the given provider id a free-tier provider?
export function isFreeTierProvider(id: string): boolean {
  return FREE_TIER_SET.has(id);
}

// Thinking config definitions
// options: list of selectable modes ("auto" = no override from server)
// defaultMode: fallback when user hasn't configured
// extended: claude-style thinking (thinking.type + budget_tokens) — used by most providers
// effort: openai-style reasoning_effort — only openai + codex
export const THINKING_CONFIG: Record<string, ThinkingConfig> = {
  extended: {
    options: ["auto", "on", "off"],
    defaultMode: "auto",
    defaultBudgetTokens: 10000
  },
  effort: {
    options: ["auto", "none", "low", "medium", "high"],
    defaultMode: "auto"
  }
};

// OAuth Providers
export const OAUTH_PROVIDERS: Record<string, Provider> = {
  claude: { id: "claude", alias: "cc", name: "Claude Code", icon: "smart_toy", color: "#D97757", website: "https://claude.ai", notice: { signupUrl: "https://claude.ai" }, priority: 10 },
  antigravity: { id: "antigravity", alias: "ag", name: "Antigravity", icon: "rocket_launch", color: "#F59E0B", deprecated: true, deprecationNotice: "AG is designed exclusively for Antigravity IDE. Using it with other tools (OpenClaw, Claude, Codex...) may result in account restrictions or bans.", website: "https://antigravity.google", notice: { signupUrl: "https://antigravity.google" }, priority: 20 },
  codex: { id: "codex", alias: "cx", name: "OpenAI Codex", icon: "code", color: "#3B82F6", thinkingConfig: THINKING_CONFIG.effort, serviceKinds: ["llm"], website: "https://chatgpt.com/codex", notice: { signupUrl: "https://chatgpt.com/codex" }, priority: 15 },
  github: { id: "github", alias: "gh", name: "GitHub Copilot", icon: "code", color: "#333333", serviceKinds: ["llm"], website: "https://github.com/features/copilot", notice: { signupUrl: "https://github.com/features/copilot" }, priority: 25, authModes: ["device_code"] },
  cursor: { id: "cursor", alias: "cu", name: "Cursor IDE", icon: "edit_note", color: "#00D4AA", website: "https://cursor.com", notice: { signupUrl: "https://cursor.com" }, priority: 30 },
  // Dual auth (OAuth device-code + API key) — merged in 68566f5; also under APIKEY_PROVIDERS.
  kimi: { id: "kimi", alias: "kimi", name: "Kimi", icon: "psychology", color: "#1E3A8A", textIcon: "KM", website: "https://kimi.moonshot.cn", notice: { signupUrl: "https://www.kimi.com/code", apiKeyUrl: "https://platform.moonshot.ai/console/api-keys" }, serviceKinds: ["llm"], authModes: ["oauth", "apikey"], hasOAuth: true, priority: 42 },
  // Dual auth (OAuth device-code + API key) — api key is primary; device flow kept for compat (ad51c2ce).
  kilocode: { id: "kilocode", alias: "kc", name: "Kilo Code", icon: "code", color: "#FF6B35", textIcon: "KC", website: "https://kilocode.ai", notice: { signupUrl: "https://kilocode.ai", apiKeyUrl: "https://kilocode.ai" }, authModes: ["oauth", "apikey"], hasOAuth: true, priority: 40 },
  cline: { id: "cline", alias: "cl", name: "Cline", icon: "smart_toy", color: "#5B9BD5", textIcon: "CL", website: "https://cline.bot", notice: { signupUrl: "https://cline.bot" }, priority: 45 },
  // Dual auth (OAuth + API key) — also listed under APIKEY_PROVIDERS for key path.
  xai: { id: "xai", alias: "xai", name: "xAI (Grok)", icon: "auto_awesome", color: "#1DA1F2", textIcon: "XA", website: "https://x.ai", notice: { apiKeyUrl: "https://console.x.ai", signupUrl: "https://accounts.x.ai" }, serviceKinds: ["llm", "imageToText"], authModes: ["oauth", "apikey"], priority: 35 },
  // Grok CLI / Grok Build device-code OAuth (cli-chat-proxy.grok.com) — distinct from xai + grok-web.
  "grok-cli": {
    id: "grok-cli",
    alias: "gcli",
    name: "Grok CLI (Grok Build)",
    icon: "auto_awesome",
    color: "#1DA1F2",
    textIcon: "GC",
    website: "https://x.ai",
    notice: {
      text: "Sign in with your xAI / Grok account via device code. Uses Grok Build subscription credits (cli-chat-proxy.grok.com).",
      signupUrl: "https://grok.com/supergrok",
    },
    serviceKinds: ["llm"],
    authModes: ["oauth"],
    priority: 36,
  },
  // opencode: { id: "opencode", alias: "oc", name: "OpenCode", icon: "terminal", color: "#E87040", textIcon: "OC" },
};

export const APIKEY_PROVIDERS: Record<string, Provider> = {
  glm: { id: "glm", alias: "glm", name: "GLM Coding", icon: "code", color: "#2563EB", textIcon: "GL", website: "https://open.bigmodel.cn", notice: { apiKeyUrl: "https://open.bigmodel.cn/usercenter/apikeys" } },
  "glm-cn": { id: "glm-cn", alias: "glm-cn", name: "GLM (China)", icon: "code", color: "#DC2626", textIcon: "GC", website: "https://open.bigmodel.cn", notice: { apiKeyUrl: "https://open.bigmodel.cn/usercenter/apikeys" } },
  kimi: { id: "kimi", alias: "kimi", name: "Kimi", icon: "psychology", color: "#1E3A8A", textIcon: "KM", website: "https://kimi.moonshot.cn", notice: { apiKeyUrl: "https://platform.moonshot.ai/console/api-keys", signupUrl: "https://www.kimi.com/code" }, serviceKinds: ["llm"], authModes: ["oauth", "apikey"], hasOAuth: true, oauth: { clientId: "17e5f671-d194-4dfb-9706-5516cb48c098", deviceCodeUrl: "https://auth.kimi.com/api/oauth/device_authorization", tokenUrl: "https://auth.kimi.com/api/oauth/token", refreshUrl: "https://auth.kimi.com/api/oauth/token" } },
  kilocode: { id: "kilocode", alias: "kc", name: "Kilo Code", icon: "code", color: "#FF6B35", textIcon: "KC", website: "https://kilocode.ai", notice: { apiKeyUrl: "https://kilocode.ai", signupUrl: "https://kilocode.ai" }, authModes: ["oauth", "apikey"], hasOAuth: true },
  minimax: { id: "minimax", alias: "minimax", name: "Minimax Coding", icon: "memory", color: "#7C3AED", textIcon: "MM", website: "https://www.minimaxi.com", notice: { apiKeyUrl: "https://platform.minimaxi.com/user-center/basic-information/interface-key" }, serviceKinds: ["llm", "imageToText"] },
  "minimax-cn": { id: "minimax-cn", alias: "minimax-cn", name: "Minimax (China)", icon: "memory", color: "#DC2626", textIcon: "MC", website: "https://www.minimaxi.com", notice: { apiKeyUrl: "https://platform.minimaxi.com/user-center/basic-information/interface-key" }, serviceKinds: ["llm"] },
  alicode: { id: "alicode", alias: "alicode", name: "Alibaba", icon: "cloud", color: "#FF6A00", textIcon: "ALi", website: "https://bailian.console.aliyun.com", notice: { apiKeyUrl: "https://bailian.console.aliyun.com/?apiKey=1" } },
  "alicode-intl": { id: "alicode-intl", alias: "alicode-intl", name: "Alibaba Intl", icon: "cloud", color: "#FF6A00", textIcon: "ALi", website: "https://modelstudio.console.alibabacloud.com", notice: { apiKeyUrl: "https://modelstudio.console.alibabacloud.com/?apiKey=1" } },
  "xiaomi-mimo": { id: "xiaomi-mimo", alias: "mimo", name: "Xiaomi MiMo", icon: "smart_toy", color: "#FF6900", textIcon: "XM", website: "https://xiaomimimo.com", notice: { apiKeyUrl: "https://xiaomimimo.com" } },
  "xiaomi-tokenplan": { id: "xiaomi-tokenplan", alias: "xmtp", name: "Xiaomi MiMo (Token Plan)", icon: "smart_toy", color: "#FF6700", textIcon: "XT", website: "https://mimo.xiaomi.com", notice: { text: "Xiaomi MiMo Token Plan subscription (API key starts with tp-). Token Plan keys are cluster-specific — select the region matching your subscription.", apiKeyUrl: "https://mimo.xiaomi.com" }, hasProviderSpecificData: true, regions: [{ id: "sgp", label: "Singapore", baseUrl: "https://token-plan-sgp.xiaomimimo.com/v1" }, { id: "cn", label: "China", baseUrl: "https://token-plan-cn.xiaomimimo.com/v1" }, { id: "ams", label: "Europe", baseUrl: "https://token-plan-ams.xiaomimimo.com/v1" }], defaultRegion: "sgp" },
  "volcengine-ark": { id: "volcengine-ark", alias: "ark", name: "Volcengine Ark", icon: "cloud", color: "#1677FF", textIcon: "ARK", website: "https://ark.cn-beijing.volces.com", notice: { apiKeyUrl: "https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey" } },
  openai: { id: "openai", alias: "openai", name: "OpenAI", icon: "auto_awesome", color: "#10A37F", textIcon: "OA", website: "https://platform.openai.com", notice: { apiKeyUrl: "https://platform.openai.com/api-keys" }, serviceKinds: ["llm", "imageToText"], thinkingConfig: THINKING_CONFIG.effort },
  anthropic: { id: "anthropic", alias: "anthropic", name: "Anthropic", icon: "smart_toy", color: "#D97757", textIcon: "AN", website: "https://console.anthropic.com", notice: { apiKeyUrl: "https://console.anthropic.com/settings/keys" }, serviceKinds: ["llm", "imageToText"] },
  "opencode-go": { id: "opencode-go", alias: "ocg", name: "OpenCode Go", icon: "terminal", color: "#E87040", textIcon: "OC", website: "https://opencode.ai/auth", notice: { text: "OpenCode Go subscription: $5/mo (then $10/mo). Access to Kimi, GLM, Qwen, MiMo, MiniMax models.", apiKeyUrl: "https://opencode.ai/auth" } },
  azure: { id: "azure", alias: "azure", name: "Azure OpenAI", icon: "cloud", color: "#0078D4", textIcon: "AZ", website: "https://azure.microsoft.com/en-us/products/ai-services/openai-service", notice: { apiKeyUrl: "https://portal.azure.com/#view/Microsoft_Azure_ProjectOxford/CognitiveServicesHub/~/OpenAI" }, hasProviderSpecificData: true },

  deepseek: { id: "deepseek", alias: "ds", name: "DeepSeek", icon: "bolt", color: "#4D6BFE", textIcon: "DS", website: "https://deepseek.com", notice: { apiKeyUrl: "https://platform.deepseek.com/api_keys" } },
  // xAI dual-auth lives under OAUTH_PROVIDERS (authModes: oauth+apikey) so it
  // is not double-listed on the API-key grid.
  mistral: { id: "mistral", alias: "mistral", name: "Mistral", icon: "air", color: "#FF7000", textIcon: "MI", website: "https://mistral.ai", notice: { apiKeyUrl: "https://console.mistral.ai/api-keys" }, serviceKinds: ["llm", "imageToText"] },
  perplexity: { id: "perplexity", alias: "pplx", name: "Perplexity", icon: "search", color: "#20808D", textIcon: "PP", website: "https://www.perplexity.ai", notice: { apiKeyUrl: "https://www.perplexity.ai/settings/api" }, serviceKinds: ["llm"] },
  together: { id: "together", alias: "together", name: "Together AI", icon: "group_work", color: "#0F6FFF", textIcon: "TG", website: "https://www.together.ai", notice: { apiKeyUrl: "https://api.together.xyz/settings/api-keys" }, serviceKinds: ["llm"] },
  fireworks: { id: "fireworks", alias: "fireworks", name: "Fireworks AI", icon: "local_fire_department", color: "#7B2EF2", textIcon: "FW", website: "https://fireworks.ai", notice: { apiKeyUrl: "https://fireworks.ai/account/api-keys" }, serviceKinds: ["llm"] },
  cerebras: { id: "cerebras", alias: "cerebras", name: "Cerebras", icon: "memory", color: "#FF4F00", textIcon: "CB", website: "https://www.cerebras.ai", notice: { apiKeyUrl: "https://cloud.cerebras.ai/platform" } },
  cohere: { id: "cohere", alias: "cohere", name: "Cohere", icon: "hub", color: "#39594D", textIcon: "CO", website: "https://cohere.com", notice: { apiKeyUrl: "https://dashboard.cohere.com/api-keys" } },
  nebius: { id: "nebius", alias: "nebius", name: "Nebius AI", icon: "cloud", color: "#6C5CE7", textIcon: "NB", website: "https://nebius.com", notice: { apiKeyUrl: "https://studio.nebius.com/settings/api-keys" }, serviceKinds: ["llm"] },
  hyperbolic: { id: "hyperbolic", alias: "hyp", name: "Hyperbolic", icon: "bolt", color: "#00D4FF", textIcon: "HY", website: "https://hyperbolic.xyz", notice: { apiKeyUrl: "https://app.hyperbolic.xyz/settings" }, serviceKinds: ["llm"] },
  "ollama-local": { id: "ollama-local", alias: "ollama-local", name: "Ollama Local", icon: "cloud", color: "#ffffffff", textIcon: "OL", website: "https://ollama.com" },
  "vertex-partner": { id: "vertex-partner", alias: "vxp", name: "Vertex Partner", icon: "cloud", color: "#34A853", textIcon: "VP", website: "https://cloud.google.com/vertex-ai/generative-ai/docs/partner-models/use-partner-models", notice: { apiKeyUrl: "https://console.cloud.google.com/iam-admin/serviceaccounts" } },
  modal: { id: "modal", alias: "modal", name: "Modal", icon: "cloud", color: "#22C55E", textIcon: "MD", website: "https://modal.com", notice: { apiKeyUrl: "https://modal.com" } },
  enally: { id: "enally", alias: "en", name: "Enally", icon: "smart_toy", color: "#EC4899", textIcon: "EN", website: "https://enally.in", notice: { apiKeyUrl: "https://ai.enally.in" } },
  llm7: { id: "llm7", alias: "llm7", name: "LLM7", icon: "psychology", color: "#7C3AED", textIcon: "L7", website: "https://llm7.io", notice: { apiKeyUrl: "https://llm7.io" } },
  longcat: { id: "longcat", alias: "lc", name: "LongCat", icon: "pets", color: "#F97316", textIcon: "LC", website: "https://longcat.chat", notice: { apiKeyUrl: "https://longcat.chat" } },
  scaleway: { id: "scaleway", alias: "scw", name: "Scaleway", icon: "cloud", color: "#4F0599", textIcon: "SC", website: "https://scaleway.com", notice: { apiKeyUrl: "https://console.scaleway.com/iam/api-keys" } },
  sambanova: { id: "sambanova", alias: "sn", name: "SambaNova", icon: "memory", color: "#FF6600", textIcon: "SN", website: "https://sambanova.ai", notice: { apiKeyUrl: "https://cloud.sambanova.ai" } },
  nscale: { id: "nscale", alias: "ns", name: "Nscale", icon: "scale", color: "#059669", textIcon: "NS", website: "https://nscale.com", notice: { apiKeyUrl: "https://console.nscale.com" } },
  "nous-research": { id: "nous-research", alias: "nous", name: "Nous Research", icon: "science", color: "#7C3AED", textIcon: "NR", website: "https://nousresearch.com", notice: { apiKeyUrl: "https://nousresearch.com" } },
  "kilo-gateway": { id: "kilo-gateway", alias: "kgw", name: "Kilo Gateway", icon: "gateway", color: "#FF6B35", textIcon: "KG", website: "https://kilo.ai", notice: { apiKeyUrl: "https://kilo.ai" } },
  modelscope: { id: "modelscope", alias: "ms", name: "ModelScope", icon: "hub", color: "#FF6A00", textIcon: "MS", website: "https://modelscope.cn", notice: { apiKeyUrl: "https://modelscope.cn/my/myaccesstoken", text: "Free tier: 500 RPD per model, 2,000 RPD total. Alibaba account or Chinese phone required." } },
  aion: { id: "aion", alias: "aion", name: "Aion Labs", icon: "science", color: "#7C3AED", textIcon: "AL", website: "https://www.aionlabs.ai", notice: { apiKeyUrl: "https://www.aionlabs.ai/pricing", text: "Free tier: Daily token allowance. No credit card required." } },
  agnes: { id: "agnes", alias: "agnes", name: "Agnes AI", icon: "favorite", color: "#EC4899", textIcon: "AG", website: "https://agnes-ai.com", notice: { apiKeyUrl: "https://apihub.agnes-ai.com", text: "Free tier: 5 free models available. Registration required." } },
  ai21: { id: "ai21", alias: "ai21", name: "AI21 Labs", icon: "psychology", color: "#2563EB", textIcon: "AI", website: "https://www.ai21.com", notice: { apiKeyUrl: "https://studio.ai21.com", text: "$10 free credit for 3 months (3-month expiry). Jamba models with 256K context." } },
  "ovhcloud": { id: "ovhcloud", alias: "ovh", name: "OVHcloud AI Endpoints", icon: "cloud", color: "#1289CD", textIcon: "OVH", website: "https://www.ovhcloud.com", notice: { apiKeyUrl: "https://endpoints.ai.cloud.ovh.net/", text: "Free tier: 2 RPM anonymous, 400 RPM with auth. Registration required." } },
};

// Web Cookie Providers (use browser session cookie instead of API key)
export const WEB_COOKIE_PROVIDERS: Record<string, Provider> = {
  "grok-web": { id: "grok-web", alias: "gw", name: "Grok Web (Subscription)", icon: "auto_awesome", color: "#1DA1F2", textIcon: "GW", website: "https://grok.com", authType: "cookie", authHint: "Paste your sso= cookie value from grok.com", passthroughModels: true, serviceKinds: ["llm"] },
};

export const OPENAI_COMPATIBLE_PREFIX = "openai-compatible-";
export const ANTHROPIC_COMPATIBLE_PREFIX = "anthropic-compatible-";

export function isOpenAICompatibleProvider(providerId: string): boolean {
  return typeof providerId === "string" && providerId.startsWith(OPENAI_COMPATIBLE_PREFIX);
}

export function isAnthropicCompatibleProvider(providerId: string): boolean {
  return typeof providerId === "string" && providerId.startsWith(ANTHROPIC_COMPATIBLE_PREFIX);
}

// All providers (combined)
export const AI_PROVIDERS: Record<string, Provider> = { ...FREE_PROVIDERS, ...FREE_TIER_PROVIDERS, ...OAUTH_PROVIDERS, ...APIKEY_PROVIDERS, ...WEB_COOKIE_PROVIDERS };

// Free-tier limitations per provider. Sourced from the awesome-freellm-apis
// directory (github.com/open-free-llm-api) and freellmapi.co, verified
// 2026-08-27. Rate limits are provider-reported and may change; treat as a
// planning guide, not a contract. Attached to each provider below so the
// dashboard can surface caps/caveats on the provider page.
export const FREE_TIER_INFO: Record<string, FreeTierInfo> = {
  nvidia: {
    accessModel: "Permanent free tier",
    creditCard: "phone",
    rateLimit: "Up to 40 RPM (varies by model)",
    maxContext: "1M tokens",
    freeModels: 126,
    productionAllowed: false,
    caveats: [
      "NVIDIA Developer Program membership required (phone verification).",
      "Trial ToS scopes usage to evaluation/prototyping, not production.",
      "Rate limit replaced depleting trial credits (verified June 2026).",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  modelscope: {
    accessModel: "Permanent free tier",
    creditCard: "registration",
    rateLimit: "2,000 RPD total; ≤500 RPD per model",
    maxContext: "1M tokens",
    freeModels: 58,
    productionAllowed: true,
    caveats: [
      "Alibaba account or Chinese phone number required.",
      "Per-model quota is ~500 RPD; total shared 2,000 RPD.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  "cloudflare-ai": {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "10K neurons/day (shared)",
    maxContext: "10M tokens (varies by model)",
    freeModels: 40,
    productionAllowed: true,
    caveats: [
      "Requires a Cloudflare API token and Account ID.",
      "Neuron-based metering is shared across your account, not per model.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  openrouter: {
    accessModel: "Renewable credits",
    creditCard: "registration",
    rateLimit: "Free tier: ~200 RPD; +$10 top-up → 1,000 RPD",
    maxContext: "1M tokens",
    freeModels: 28,
    productionAllowed: true,
    caveats: [
      "Free tier is rate-limited to ~200 requests/day.",
      "Anthropic models need a one-time $10 top-up to unlock.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  gemini: {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "15 RPM, 1,500 RPD (gemini-3.6-flash)",
    maxContext: "1M tokens",
    freeModels: 17,
    productionAllowed: true,
    caveats: [
      "Gemini 3.5 Flash-Lite is more generous: 30 RPM, 1,500 RPD.",
      "Quotas apply per API key via AI Studio.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  ollama: {
    accessModel: "Permanent free tier",
    creditCard: "registration",
    rateLimit: "Session/weekly limits (~5–10M tokens/mo)",
    maxContext: "1M tokens",
    freeModels: 13,
    productionAllowed: false,
    caveats: [
      "1 cloud model at a time; limits reset every 5h & 7d.",
      "Pro $20/mo · Max $100/mo for heavier use.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  vertex: {
    accessModel: "Trial credits",
    creditCard: "required",
    rateLimit: "Limited by $300 new-account credit",
    maxContext: "1M tokens",
    productionAllowed: false,
    caveats: [
      "New Google Cloud accounts get $300 free credits (card required).",
      "Needs GCP project + Service Account with Vertex AI API enabled.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  github: {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "Rate-limited (see GitHub Models)",
    maxContext: "1M tokens",
    freeModels: 16,
    productionAllowed: true,
    caveats: [
      "Sign in with a GitHub account (no card).",
      "Rate limits are applied per model family.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  "opencode-zen": {
    accessModel: "Permanent free tier",
    creditCard: "registration",
    rateLimit: "Rate-limited (see OpenCode Zen)",
    maxContext: "1M tokens",
    freeModels: 12,
    productionAllowed: true,
    caveats: ["OpenCode account required (registration)."],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  kilocode: {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "~200 req/hr",
    maxContext: "1M tokens",
    freeModels: 12,
    productionAllowed: true,
    caveats: ["Kilo Code account (free) grants gateway access."],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  aion: {
    accessModel: "Permanent free tier",
    creditCard: "registration",
    rateLimit: "15 RPM, 20K TPD",
    maxContext: "131K tokens",
    freeModels: 7,
    productionAllowed: true,
    caveats: [
      "Aion Labs account required (registration, no card).",
      "Daily token allowance resets each day.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  agnes: {
    accessModel: "Permanent free tier",
    creditCard: "registration",
    rateLimit: "30 RPM",
    maxContext: "256K tokens",
    freeModels: 5,
    productionAllowed: true,
    caveats: [
      "Registration required (platform.agnes-ai.com).",
      "5 free models available; image model at 4K context.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  ai21: {
    accessModel: "Trial credits",
    creditCard: "registration",
    rateLimit: "Limited by $10 / 3-month credit",
    maxContext: "256K tokens",
    freeModels: 2,
    productionAllowed: false,
    caveats: [
      "$10 free credit for 3 months (expires after 3 months).",
      "Jamba models with 256K context.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  ovhcloud: {
    accessModel: "Permanent free tier",
    creditCard: "registration",
    rateLimit: "2 RPM anonymous · 400 RPM with auth",
    maxContext: "262K tokens",
    freeModels: 14,
    productionAllowed: true,
    caveats: [
      "Registration required; anonymous access limited to 2 RPM.",
      "Authenticated requests raise the limit to ~400 RPM.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  mistral: {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "~30 RPM (varies by model); ~1 RPS on mistral-medium-3.5-128b",
    maxContext: "256K tokens",
    freeModels: 12,
    productionAllowed: true,
    caveats: [
      "No credit card required. Includes open-mistral-7b, open-mixtral-8x7b, and the Mistral free tier.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  llm7: {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "30 RPM (120 RPM with token auth)",
    maxContext: "1M tokens",
    freeModels: 16,
    productionAllowed: true,
    caveats: [
      "No credit card required. Free tier covers DeepSeek, GPT-OSS, Qwen, and GPT-4o Mini. Anonymous = 30 RPM; authenticated = 120 RPM.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  sambanova: {
    accessModel: "Permanent free tier",
    creditCard: "registration",
    rateLimit: "20 RPM, 20 RPD, 200K TPD",
    maxContext: "128K tokens",
    freeModels: 4,
    productionAllowed: true,
    caveats: [
      "Registration required (no card). Daily token allowance resets each day.",
      "Models: DeepSeek-V3.1, DeepSeek-V3.2 Preview, MiniMax-M2.7, Llama-3.3 70B.",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
  kiro: {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "50 credits/month (open-weight models + Claude Sonnet 4.5); upgrades start at $20/mo",
    maxContext: "1M tokens",
    freeModels: 12,
    productionAllowed: false,
    caveats: [
      "Free tier: 50 credits/month, no card for social/AWS Builder ID sign-up.",
      "Access to open-weight models (Qwen3 Coder Next, DeepSeek 3.2, MiniMax M2.1) and Claude Sonnet 4.5, subject to rate limits.",
      "Not available in AWS GovCloud (US).",
    ],
    lastVerified: "2026-08-27",
    source: "https://kiro.dev/pricing/",
  },
  huggingface: {
    accessModel: "Permanent free tier",
    creditCard: "none",
    rateLimit: "~few hundred requests/hour (rate-limited; PRO $9/mo for higher limits)",
    maxContext: "Varies by model",
    freeModels: 7,
    productionAllowed: false,
    caveats: [
      "No credit card required. Serverless Inference API is free, rate-limited per user.",
      "Thousands of open models available; popular free models include Llama, Qwen, and Gemma variants.",
      "Higher rate limits / longer contexts require a PRO subscription ($9/mo) or dedicated Inference Endpoints (paid).",
    ],
    lastVerified: "2026-08-27",
    source: "https://github.com/open-free-llm-api/awesome-freellm-apis",
  },
};

// Attach the free-tier flag + limitations to every provider that has limit data
// (decoupled from FREE_TIER_PROVIDER_IDS so adding limits never changes the
// dashboard's free-tier categorization / green-dot filter). AI_PROVIDERS
// entries are the same object references as the source maps.
for (const id of Object.keys(FREE_TIER_INFO)) {
  const provider = AI_PROVIDERS[id];
  const info = FREE_TIER_INFO[id];
  if (provider && info) {
    provider.freeTier = true;
    provider.freeTierInfo = info;
  }
}

// Auth methods
export const AUTH_METHODS: Record<string, AuthMethod> = {
  oauth: { id: "oauth", name: "OAuth", icon: "lock" },
  apikey: { id: "apikey", name: "API Key", icon: "key" },
  cookie: { id: "cookie", name: "Browser Cookie", icon: "cookie" },
};

// Helper: Get provider by alias
export function getProviderByAlias(alias: string): Provider | null {
  for (const provider of Object.values(AI_PROVIDERS)) {
    if (provider.alias === alias || provider.id === alias) {
      return provider;
    }
  }
  return null;
}

// Helper: Get provider ID from alias
export function resolveProviderId(aliasOrId: string): string {
  const provider = getProviderByAlias(aliasOrId);
  return provider?.id || aliasOrId;
}

// Helper: Get alias from provider ID
export function getProviderAlias(providerId: string): string {
  const provider = AI_PROVIDERS[providerId];
  return provider?.alias || providerId;
}

// Alias to ID mapping (for quick lookup)
export const ALIAS_TO_ID: Record<string, string> = Object.values(AI_PROVIDERS).reduce((acc, p) => {
  acc[p.alias] = p.id;
  return acc;
}, {} as Record<string, string>);

// ID to Alias mapping
export const ID_TO_ALIAS: Record<string, string> = Object.values(AI_PROVIDERS).reduce((acc, p) => {
  acc[p.id] = p.alias;
  return acc;
}, {} as Record<string, string>);

// Providers that support usage/quota API
export const USAGE_SUPPORTED_PROVIDERS: string[] = [
  "claude",
  "antigravity",
  "kiro",
  "github",
  "codex",
  "kimi",
  "kimi-coding",
  "deepseek",
  "ollama",
  "grok-cli",
  "glm",
  "glm-cn",
  "minimax",
  "minimax-cn",
];

// Subset that uses apikey auth (still surfaced on quota page)
export const USAGE_APIKEY_PROVIDERS: string[] = [
  "glm",
  "glm-cn",
  "minimax",
  "minimax-cn",
  "kimi",
  "deepseek",
];

// Providers whose dashboard exposes an "Import catalog" button — mirrors
// src/server/api/provider_models.rs supports_models_discovery.
export const SUPPORTS_MODELS_DISCOVERY: string[] = [
  "openai",
  "openrouter",
  "anthropic",
  "claude",
  "gemini",
  "nvidia",
  "llm7",
  "deepseek",
  "xai",
  "mistral",
  "perplexity",
  "together",
  "fireworks",
  "cerebras",
  "cohere",
  "nebius",
  "hyperbolic",
  "ollama",
  "opencode-zen",
  "codex",
  "antigravity",
  "github",
  "qwen",
  "alicode",
  "alicode-intl",
  "volcengine-ark",
  "nanobanana",
  "assemblyai",
  "modal",
  "longcat",
  "scaleway",
  "sambanova",
  "nscale",
  "nous-research",
  "glhf",
  "kilocode",
  "modelscope",
  "aion",
  "agnes",
  "ai21",
  "ovhcloud",
];
