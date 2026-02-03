export interface ApiResponse<T = unknown> {
  success: boolean;
  data?: T;
  error?: string;
}

export interface HealthData {
  status: "ok";
  version: string;
  uptime: number;
}
