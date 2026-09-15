// Shared TypeScript types for the OpenProxy Astro dashboard

// Theme types
export type Theme = "light" | "dark" | "system";

// Dashboard API key (GET /api/keys).
export interface ApiKey {
  id: string;
  name: string;
  key: string;
  createdAt: string;
  isActive?: boolean;
}

// Provider types
export interface Provider {
  id: string;
  alias: string;
  name: string;
  icon: string;
  color: string;
  textIcon?: string;
  website?: string;
  notice?: {
    text?: string;
    signupUrl?: string;
    apiKeyUrl?: string;
  };
  deprecated?: boolean;
  deprecationNotice?: string;
  noAuth?: boolean;
  passthroughModels?: boolean;
  modelsFetcher?: {
    url: string;
    type: string;
  };
  serviceKinds?: ServiceKind[];
  thinkingConfig?: ThinkingConfig;
  hasProviderSpecificData?: boolean;
  hidden?: boolean;
  /** Display sort weight (lower first). Mirrors 9router registry priority. */
  priority?: number;
  authType?: "oauth" | "apikey" | "cookie";
  /**
   * When a provider supports more than one auth path (e.g. xAI OAuth + API key),
   * list every allowed mode. Dual providers also appear in OAUTH_PROVIDERS and
   * may still live in APIKEY_PROVIDERS for catalog/list grouping.
   */
  authModes?: Array<"oauth" | "apikey" | "cookie">;
  hasOAuth?: boolean;
  oauth?: {
    clientId?: string;
    deviceCodeUrl?: string;
    tokenUrl?: string;
    refreshUrl?: string;
  };
  authHint?: string;
  /** True when the provider is part of the free-tier set (see FREE_TIER_PROVIDER_IDS). */
  freeTier?: boolean;
  /**
   * Structured free-tier limitations shown on the provider page. Populated for
   * free-tier providers so users can see rate limits, caps, and caveats before
   * configuring a connection. Data sourced from awesome-freellm-apis / freellmapi.
   */
  freeTierInfo?: FreeTierInfo;
}

// Free-tier provider limitations shown on the provider page. Data is sourced
// from awesome-freellm-apis (github.com/open-free-llm-api) and freellmapi.co,
// refreshed 2026-08-27. Rate limits are kept free-form because units differ
// across providers (RPM / RPD / TPD / tokens-per-month).
export interface FreeTierInfo {
  /** How the free access works, e.g. "Permanent free tier" or "Renewable credits". */
  accessModel: string;
  /** What signup requires to obtain a key. */
  creditCard: "none" | "registration" | "phone" | "required";
  /** Free-form rate limit summary (e.g. "30 RPM, 14,400 RPD"). */
  rateLimit?: string;
  /** Max context window of free models (e.g. "1M tokens"). */
  maxContext?: string;
  /** Number of models available on the free tier (if known). */
  freeModels?: number;
  /** Whether production use is allowed under the free-tier ToS. */
  productionAllowed?: boolean;
  /** Caveats / quirks worth surfacing (eval-only ToS, session limits, etc.). */
  caveats?: string[];
  /** ISO date the data was last verified. */
  lastVerified?: string;
  /** Source URL for the data. */
  source?: string;
}

export type ServiceKind = "llm" | "imageToText";

export interface ThinkingConfig {
  options: string[];
  defaultMode: string;
  defaultBudgetTokens?: number;
}

export interface AuthMethod {
  id: string;
  name: string;
  icon: string;
}

// Model types
export interface Model {
  id: string;
  name: string;
  provider?: string;
  context?: number;
  pricing?: ModelPricing;
}

export interface ModelPricing {
  input?: number;
  output?: number;
  unit?: string;
}

// Store types
export interface ThemeStore {
  theme: Theme;
  setTheme: (theme: Theme) => void;
  toggleTheme: () => void;
  initTheme: () => void;
}

export interface UserStore {
  user: User | null;
  setUser: (user: User | null) => void;
  clearUser: () => void;
}

export interface User {
  id: string;
  email?: string;
  name?: string;
  avatar?: string;
}

export interface ProviderStore {
  providers: Provider[];
  connections: Connection[];
  addConnection: (connection: Connection) => void;
  removeConnection: (id: string) => void;
  updateConnection: (id: string, updates: Partial<Connection>) => void;
}

export interface Connection {
  id: string;
  providerId: string;
  apiKey?: string;
  settings?: Record<string, unknown>;
  createdAt: string;
  updatedAt: string;
}

export interface NotificationStore {
  notifications: Notification[];
  addNotification: (notification: Omit<Notification, "id">) => void;
  removeNotification: (id: string) => void;
  clearNotifications: () => void;
}

export interface Notification {
  id: string;
  type: "success" | "error" | "warning" | "info";
  title: string;
  message?: string;
  duration?: number;
}

// API types
export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
}

export interface ApiError {
  message: string;
  code?: string;
  status?: number;
}

// Component prop types
export interface ButtonProps extends React.ButtonHTMLAttributes<HTMLButtonElement> {
  /**
   * Visual style. Claude editorial system uses scarce coral CTAs +
   * cream-canvas secondaries; `primary-on-dark` / `secondary-on-dark`
   * are reserved for content sitting on dark navy product surfaces.
   */
  variant?:
    | "primary"
    | "secondary"
    | "outline"
    | "ghost"
    | "danger"
    | "success"
    | "primary-on-dark"
    | "secondary-on-dark";
  size?: "sm" | "md" | "lg";
  icon?: string;
  iconRight?: string;
  disabled?: boolean;
  loading?: boolean;
  fullWidth?: boolean;
  children?: React.ReactNode;
}

export interface InputProps extends React.InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  error?: string;
  hint?: string;
  icon?: string;
  iconRight?: string;
  inputClassName?: string;
}

export interface ModalProps {
  isOpen: boolean;
  onClose: () => void;
  title?: string;
  children?: React.ReactNode;
  size?: "sm" | "md" | "lg" | "xl";
}

// Utility types
export type ClassNameValue = string | number | boolean | undefined | null | ClassNameValue[];
