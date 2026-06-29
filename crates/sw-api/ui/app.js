"use strict";
// No-build SPA. Every node is built with createElement + textContent — never
// innerHTML — so untrusted run/log/error data can never be parsed as HTML.

// ---- session token --------------------------------------------------------
const TOKEN = (() => {
  const url = new URL(window.location.href);
  const fromUrl = url.searchParams.get("token");
  if (fromUrl) {
    sessionStorage.setItem("wxd_token", fromUrl);
    url.searchParams.delete("token");
    window.history.replaceState({}, "", url.toString());
    return fromUrl;
  }
  return sessionStorage.getItem("wxd_token") || "";
})();

// ---- tiny DOM helpers -----------------------------------------------------
function $(sel) {
  return document.querySelector(sel);
}
function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.class) node.className = opts.class;
  if (opts.text != null) node.textContent = opts.text;
  if (opts.attrs) for (const [k, v] of Object.entries(opts.attrs)) node.setAttribute(k, v);
  for (const c of children) if (c) node.appendChild(c);
  return node;
}
function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
}

// ---- API ------------------------------------------------------------------
async function api(path, options = {}) {
  const headers = Object.assign({ "x-wxd-token": TOKEN }, options.headers || {});
  if (options.body) headers["content-type"] = "application/json";
  const res = await fetch(`/api${path}`, Object.assign({}, options, { headers }));
  if (!res.ok) throw new Error(`${options.method || "GET"} ${path} → ${res.status}`);
  const ct = res.headers.get("content-type") || "";
  return ct.includes("application/json") ? res.json() : res.text();
}

// ---- state ----------------------------------------------------------------
let currentRunId = null;
let eventSource = null;

// ---- theme ----------------------------------------------------------------
(function initTheme() {
  const saved = localStorage.getItem("wxd_theme");
  if (saved) document.documentElement.setAttribute("data-theme", saved);
  $("#theme-toggle").addEventListener("click", () => {
    const cur = document.documentElement.getAttribute("data-theme");
    const next = cur === "dark" ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", next);
    localStorage.setItem("wxd_theme", next);
  });
})();

// ---- banner ---------------------------------------------------------------
function banner(kind, message) {
  const b = $("#status-banner");
  b.hidden = false;
  b.className = `banner ${kind}`;
  b.textContent = message;
}

// ---- catalog --------------------------------------------------------------
async function loadCatalog() {
  try {
    const [hs, svcs] = await Promise.all([
      api("/catalog/hyperscalers"),
      api("/catalog/services"),
    ]);
    const hsList = $("#hyperscalers");
    clear(hsList);
    for (const h of hs) {
      const cls = "chip" + (h.enabled ? "" : " disabled");
      const label = h.enabled ? h.name : `${h.name} (coming soon)`;
      hsList.appendChild(el("li", { class: cls, text: label }));
    }
    const svcList = $("#services");
    clear(svcList);
    for (const s of svcs) {
      const cls = "chip" + (s.default_selected ? " default" : "");
      const label = s.default_selected ? `${s.name} (default)` : s.name;
      svcList.appendChild(el("li", { class: cls, text: label }));
    }
  } catch (e) {
    banner("fail", `Could not load catalog: ${e.message}`);
  }
}

// ---- run rendering --------------------------------------------------------
function renderRun(run) {
  $("#run-meta").textContent = `Run ${run.id} — ${run.status}`;

  const steps = $("#steps");
  clear(steps);
  for (const s of run.steps) {
    const dot = el("span", { class: "dot", attrs: { "aria-hidden": "true" } });
    const title = el("span", { class: "title", text: s.title });
    const state = el("span", { class: "state", text: s.status.replace(/_/g, " ") });
    const li = el("li", { class: `step ${s.status}` }, [dot, title, state]);

    if (s.status === "failed" && s.error) {
      const errBox = el("div", { class: "step-error" }, [
        el("div", { text: s.error }),
      ]);
      if (s.next_steps && s.next_steps.length) {
        const ul = el("ul");
        for (const ns of s.next_steps) ul.appendChild(el("li", { text: ns }));
        errBox.appendChild(ul);
      }
      li.appendChild(errBox);
    }
    steps.appendChild(li);
  }

  // Control buttons reflect run status.
  $("#pause-btn").disabled = run.status !== "running";
  $("#resume-btn").disabled = run.status !== "paused";
  $("#retry-btn").disabled = run.status !== "failed";
  // Destroy is available once a cluster may exist (i.e. provisioning ran).
  const provisionRan = run.steps.some(
    (s) => s.id === "mod-provision/create-cluster" && s.status === "completed"
  );
  $("#destroy-btn").disabled = !provisionRan;

  // Input panel.
  const inputPanel = $("#input-panel");
  if (run.status === "awaiting_input" && run.pending_inputs && run.pending_inputs.length) {
    renderInputForm(run);
    inputPanel.hidden = false;
  } else {
    inputPanel.hidden = true;
  }

  if (run.status === "completed") banner("ok", "Install completed.");
  else if (run.status === "failed") banner("fail", "A step failed — see the error and retry.");
  else if (run.status === "paused") banner("info", "Paused. Resume when ready.");
}

function renderInputForm(run) {
  $("#input-prompt").textContent = run.pending_prompt || "Please provide the following:";
  const form = $("#input-form");
  clear(form);

  for (const f of run.pending_inputs) {
    const input = el("input", {
      attrs: {
        name: f.key,
        type: f.secret ? "password" : "text",
        autocomplete: f.secret ? "new-password" : "off",
      },
    });
    if (f.default != null) input.value = f.default;
    input.dataset.secret = f.secret ? "1" : "0";
    const label = el("label", {}, [
      el("span", { text: f.label }),
      input,
    ]);
    form.appendChild(label);
  }
  form.appendChild(el("button", { class: "primary", text: "Submit", attrs: { type: "submit" } }));

  form.onsubmit = async (ev) => {
    ev.preventDefault();
    const values = {};
    const secrets = {};
    for (const input of form.querySelectorAll("input")) {
      if (input.dataset.secret === "1") secrets[input.name] = input.value;
      else values[input.name] = input.value;
    }
    try {
      await api(`/runs/${run.id}/inputs`, {
        method: "POST",
        body: JSON.stringify({ values, secrets }),
      });
      $("#input-panel").hidden = true;
    } catch (e) {
      banner("fail", `Could not submit inputs: ${e.message}`);
    }
  };
}

async function refreshRun() {
  if (!currentRunId) return;
  try {
    const run = await api(`/runs/${currentRunId}`);
    renderRun(run);
  } catch (e) {
    banner("fail", `Could not load run: ${e.message}`);
  }
}

// ---- live events ----------------------------------------------------------
function connectEvents(id) {
  if (eventSource) eventSource.close();
  const log = $("#log");
  eventSource = new EventSource(`/api/runs/${id}/events?token=${encodeURIComponent(TOKEN)}`);
  eventSource.onmessage = (msg) => {
    let ev;
    try {
      ev = JSON.parse(msg.data);
    } catch {
      return;
    }
    if (ev.kind === "log") {
      log.appendChild(document.createTextNode(`[${ev.step}] ${ev.line}\n`));
      log.scrollTop = log.scrollHeight;
    } else if (ev.kind === "step_status" || ev.kind === "run_status" || ev.kind === "progress") {
      // Authoritative state lives server-side; re-pull on any status change.
      if (ev.kind !== "progress") refreshRun();
    }
  };
  eventSource.onerror = () => {
    // EventSource auto-reconnects; nothing to do.
  };
}

// ---- controls -------------------------------------------------------------
function selectedMode() {
  const checked = document.querySelector('input[name="mode"]:checked');
  return checked ? checked.value : "provision";
}

// Collect non-empty cloud credentials from the credentials panel.
function collectCredentials() {
  const creds = {};
  for (const input of document.querySelectorAll("#creds-form input[data-cred]")) {
    const v = input.value.trim();
    if (v) creds[input.dataset.cred] = v;
  }
  return creds;
}

$("#start-btn").addEventListener("click", async () => {
  try {
    $("#log").textContent = "";
    const mode = selectedMode();
    const credentials = collectCredentials();
    const run = await api("/runs", {
      method: "POST",
      body: JSON.stringify({ mode, credentials }),
    });
    currentRunId = run.id;
    banner(
      "info",
      mode === "existing"
        ? "Install started against your existing cluster."
        : "Install started — provisioning a new AWS cluster."
    );
    connectEvents(run.id);
    renderRun(run);
  } catch (e) {
    banner("fail", `Could not start: ${e.message}`);
  }
});

$("#pause-btn").addEventListener("click", async () => {
  if (currentRunId) await api(`/runs/${currentRunId}/pause`, { method: "POST" }).catch(() => {});
});
$("#resume-btn").addEventListener("click", async () => {
  if (currentRunId) await api(`/runs/${currentRunId}/resume`, { method: "POST" }).catch(() => {});
});
$("#retry-btn").addEventListener("click", async () => {
  if (currentRunId) await api(`/runs/${currentRunId}/retry`, { method: "POST" }).catch(() => {});
});
$("#destroy-btn").addEventListener("click", async () => {
  if (!currentRunId) return;
  if (!window.confirm("Destroy the provisioned OpenShift cluster? This removes its AWS resources.")) return;
  banner("info", "Destroying cluster — watch the log for progress.");
  await api(`/runs/${currentRunId}/destroy`, { method: "POST" }).catch((e) =>
    banner("fail", `Could not start teardown: ${e.message}`)
  );
});

// ---- boot -----------------------------------------------------------------
loadCatalog();
