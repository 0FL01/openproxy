export interface LogTokens {
  prompt_tokens?: number;
  input_tokens?: number;
  completion_tokens?: number;
  output_tokens?: number;
  cache_read_input_tokens?: number;
  cache_creation_input_tokens?: number;
}

export interface ApplicationLog {
  id: string;
  timestamp: string;
  method: string;
  endpoint?: string;
  status: "pending" | "success" | "error" | "interrupted" | string;
  statusCode?: number;
  requestedModel?: string;
  model: string;
  provider: string;
  connectionId?: string;
  account?: string;
  apiKeyId?: string;
  apiKeyName?: string;
  correlationId?: string;
  latency?: { ttft?: number; total?: number };
  tokens?: LogTokens;
  cost?: number;
  error?: string;
}

export interface LogsPayload {
  details: ApplicationLog[];
  pagination: {
    page: number;
    pageSize: number;
    totalItems: number;
    totalPages: number;
  };
}

export const inputTokens = (tokens?: LogTokens) =>
  tokens?.prompt_tokens ?? tokens?.input_tokens ?? 0;

export const outputTokens = (tokens?: LogTokens) =>
  tokens?.completion_tokens ?? tokens?.output_tokens ?? 0;
