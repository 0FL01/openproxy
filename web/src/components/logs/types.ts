export interface ApplicationLog {
  requestId: string;
  timestamp: string;
  route: string;
  status: "pending" | "success" | "error" | "interrupted" | string;
  statusCode?: number;
  model: string;
  provider: string;
  durationMs: number;
  inputTokens?: number;
  outputTokens?: number;
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
