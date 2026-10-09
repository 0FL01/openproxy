export interface ApplicationLog {
  requestId: string;
  timestamp: string;
  route: string;
  status: "pending" | "success" | "error" | "interrupted" | string;
  statusCode?: number;
  errorKind?: string;
  errorCode?: string;
  errorMessage?: string;
  model: string;
  provider: string;
  durationMs: number;
  inputTokens?: number | null;
  outputTokens?: number | null;
  cachedTokens?: number | null;
  tokensPerSecond: number | null;
  generatedOutputTokens: number | null;
  upstreamDurationMs: number | null;
  apiKeyId?: string;
  apiKeyName?: string;
}

export interface LogsPayload {
  requests: ApplicationLog[];
  pagination: {
    page: number;
    pageSize: number;
    totalItems: number;
    totalPages: number;
  };
}
