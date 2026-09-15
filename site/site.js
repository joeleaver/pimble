// Pimble site: shared helpers for the account/API pages. Classic script
// (no bundler, no modules) so every page can load it with a plain
// <script src="site.js"></script> and call these as globals.

const PIMBLE_API_BASE = "/api/v1";

class ApiError extends Error {
  constructor(code, message, status) {
    super(message);
    this.code = code;
    this.status = status;
  }
}

// Calls the accounts API (pimble-cloud) and returns the parsed JSON body.
// Always sends the session cookie. Throws an ApiError with the server's
// { error, message } shape on a non-2xx response, or a "network" ApiError
// when the request never got a response at all -- which is exactly what
// happens when this site is previewed locally (python3 -m http.server)
// without pimble-cloud running behind it.
async function apiFetch(path, options) {
  options = options || {};
  const headers = Object.assign(
    {},
    options.body ? { "Content-Type": "application/json" } : {},
    options.headers || {}
  );
  let res;
  try {
    res = await fetch(PIMBLE_API_BASE + path, {
      credentials: "same-origin",
      ...options,
      headers,
    });
  } catch (err) {
    throw new ApiError("network", "Couldn't reach the Pimble server.");
  }
  let body = null;
  try {
    body = await res.json();
  } catch (err) {
    // No JSON body -- e.g. a plain 404 from a static file server when this
    // page is previewed without the accounts API behind it.
  }
  if (!res.ok) {
    const code = (body && body.error) || "error";
    const message = (body && body.message) || `Request failed (${res.status}).`;
    throw new ApiError(code, message, res.status);
  }
  return body;
}

function escapeHtml(str) {
  return String(str).replace(/[&<>"']/g, (c) => (
    { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]
  ));
}

function formatDate(iso) {
  try {
    return new Date(iso).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  } catch (err) {
    return iso;
  }
}

function formatBytes(n) {
  if (typeof n !== "number" || Number.isNaN(n)) return "";
  const units = ["B", "KB", "MB", "GB"];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v >= 10 || i === 0 ? 0 : 1)} ${units[i]}`;
}

function setFormError(form, message) {
  const el = form.querySelector("[data-form-error]");
  if (!el) return;
  el.textContent = message || "";
  el.hidden = !message;
}
