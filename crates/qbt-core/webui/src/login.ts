// Login page. On success the server sets the HttpOnly session cookie and
// the same URL then serves the signed-in UI (no redirect target is taken
// from the URL).

const read = (k: string, d: string): string => {
  try {
    const v = localStorage.getItem("neors." + k);
    return v ? (JSON.parse(v) as string) : d;
  } catch {
    return d;
  }
};
const root = document.documentElement;
const theme = read("theme", "auto");
if (theme !== "auto") root.dataset.theme = theme;
root.dataset.accent = read("accent", "blue");

const form = document.getElementById("login") as HTMLFormElement;
const err = document.getElementById("error") as HTMLElement;
const btn = form.querySelector("button") as HTMLButtonElement;

form.addEventListener("submit", async (e) => {
  e.preventDefault();
  btn.disabled = true;
  err.textContent = "";
  try {
    const r = await fetch("api/v2/auth/login", {
      method: "POST",
      credentials: "same-origin",
      cache: "no-store",
      body: new URLSearchParams(new FormData(form) as unknown as Record<string, string>),
    });
    const text = (await r.text()).trim();
    if (r.ok && text !== "Fails.") {
      location.reload();
      return;
    }
    err.textContent = r.status === 403 ? text || "Too many failed attempts. Try again later." : "Incorrect username or password.";
  } catch {
    err.textContent = "Can’t reach the server.";
  }
  btn.disabled = false;
  form.classList.remove("shake");
  void form.offsetWidth;
  form.classList.add("shake");
  (form.elements.namedItem("password") as HTMLInputElement).select();
});
