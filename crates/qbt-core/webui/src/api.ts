// WebAPI v2 client. Relative URLs keep the UI working behind a reverse proxy
// sub-path; cookies stay same-origin (the SID cookie is HttpOnly).

export type Params = Record<string, string | number | boolean | undefined>;

export class ApiError extends Error {
  constructor(public status: number, message: string) {
    super(message || `HTTP ${status}`);
  }
}

let onAuthLost = () => location.reload();
export const setAuthLost = (f: () => void) => (onAuthLost = f);

function qs(p?: Params): string {
  if (!p) return "";
  const u = new URLSearchParams();
  for (const k in p) if (p[k] !== undefined) u.set(k, String(p[k]));
  return u.toString();
}

async function send(path: string, init: RequestInit): Promise<Response> {
  const r = await fetch("api/v2/" + path, { credentials: "same-origin", cache: "no-store", ...init });
  if (r.status === 403 && !path.startsWith("auth/")) {
    onAuthLost();
    throw new ApiError(403, "Session expired");
  }
  if (!r.ok) throw new ApiError(r.status, (await r.text().catch(() => "")).slice(0, 300));
  return r;
}

async function body(r: Response): Promise<any> {
  const t = await r.text();
  if (!t) return null;
  return (r.headers.get("content-type") ?? "").includes("json") ? JSON.parse(t) : t;
}

export async function get<T = any>(path: string, p?: Params): Promise<T> {
  const q = qs(p);
  return body(await send(q ? `${path}?${q}` : path, { method: "GET" }));
}

/** POST as a form (urlencoded) or multipart (`FormData`). */
export async function post<T = any>(path: string, p?: Params | FormData): Promise<T> {
  const b = p instanceof FormData ? p : new URLSearchParams(qs(p));
  return body(await send(path, { method: "POST", body: b }));
}
