export interface StreamTrace {
  version: number;
  upstreamEvents?: Record<string, number>;
  emittedEvents?: Record<string, number>;
  itemTypes?: Record<string, number>;
  toolNames?: string[];
  stopReason?: string;
  finishReason?: string;
  completedCount: number;
  errorCount: number;
  framesAfterCompleted: number;
  doneSent: boolean;
  overflowed?: number;
  entries: string[];
}

export interface ApplicationLog {
  requestId: string;
  timestamp: string;
  route: string;
  status: "pending" | "success" | "error" | "interrupted" | string;
  statusCode?: number;
  errorKind?: string;
  errorCode?: string;
  errorMessage?: string;
  streamTrace?: StreamTrace;
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

export type TpsStatsWindow = "1h" | "6h" | "24h";
export type TpsStatsGroupId = string | null;

export interface TpsStatsObservation {
  medianTps: number | null;
  sampleCount: number;
  successfulAttemptCount: number;
}

export interface TpsStatsGroup {
  provider: TpsStatsGroupId;
  model: TpsStatsGroupId;
  windows: Record<TpsStatsWindow, TpsStatsObservation>;
}

export interface TpsStatsPayload {
  asOf: string;
  windows: Record<TpsStatsWindow, { start: string; end: string }>;
  groups: TpsStatsGroup[];
  pagination: {
    page: number;
    pageSize: number;
    totalItems: number;
    totalPages: number;
  };
}
