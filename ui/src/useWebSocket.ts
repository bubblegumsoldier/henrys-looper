import { useEffect, useRef, useState } from "react";
import type { LooperEvent } from "./types";

export type WsStatus = "connecting" | "open" | "closed";

/** WebSocket client with exponential-backoff auto-reconnect. */
export function useWebSocket(onEvent: (ev: LooperEvent) => void): WsStatus {
  const [status, setStatus] = useState<WsStatus>("connecting");
  const handler = useRef(onEvent);
  handler.current = onEvent;

  useEffect(() => {
    let ws: WebSocket | null = null;
    let timer: number | undefined;
    let attempt = 0;
    let disposed = false;

    const connect = () => {
      if (disposed) return;
      const proto = location.protocol === "https:" ? "wss:" : "ws:";
      const url = `${proto}//${location.host}/ws`;
      setStatus("connecting");
      try {
        ws = new WebSocket(url);
      } catch {
        schedule();
        return;
      }
      ws.onopen = () => {
        attempt = 0;
        setStatus("open");
      };
      ws.onmessage = (m) => {
        try {
          const ev = JSON.parse(m.data as string) as LooperEvent;
          if (ev && typeof ev === "object" && "type" in ev) handler.current(ev);
        } catch {
          /* ignore malformed frames */
        }
      };
      ws.onerror = () => {
        /* onclose follows */
      };
      ws.onclose = () => {
        setStatus("closed");
        schedule();
      };
    };

    const schedule = () => {
      if (disposed) return;
      const delay = Math.min(5000, 500 * 2 ** Math.min(attempt, 4));
      attempt += 1;
      timer = window.setTimeout(connect, delay);
    };

    connect();
    return () => {
      disposed = true;
      if (timer) window.clearTimeout(timer);
      if (ws) {
        ws.onclose = null;
        ws.close();
      }
    };
  }, []);

  return status;
}
