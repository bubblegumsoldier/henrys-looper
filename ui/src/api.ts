import type { CompileResult, LoadResult, StateEvent } from "./types";

export class ApiError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(path, {
      headers: { "Content-Type": "application/json" },
      ...init,
    });
  } catch (e) {
    throw new ApiError(0, `Backend nicht erreichbar (${(e as Error).message})`);
  }
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    data = text;
  }
  if (!res.ok) {
    const detail =
      data && typeof data === "object" && "detail" in (data as Record<string, unknown>)
        ? String((data as Record<string, unknown>).detail)
        : `HTTP ${res.status}`;
    throw new ApiError(res.status, detail);
  }
  return data as T;
}

export const api = {
  compile: (yaml: string) => request<CompileResult>("/api/compile", { method: "POST", body: JSON.stringify({ yaml }) }),
  load: (yaml: string) => request<LoadResult>("/api/load", { method: "POST", body: JSON.stringify({ yaml }) }),
  transport: (action: "start" | "stop_all" | "next") =>
    request<{ ok: boolean; state: StateEvent }>(`/api/transport/${action}`, { method: "POST" }),
  state: () => request<StateEvent>("/api/state"),
  score: () => request<{ yaml: string; score: unknown; stub: boolean }>("/api/score"),
  engines: () => request<{ current: string; connected: boolean; available: string[] }>("/api/engines"),
  setEngine: (engine: string) =>
    request<{ current: string; connected: boolean }>("/api/engines", { method: "POST", body: JSON.stringify({ engine }) }),
};
