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
function effectiveTheme() {
  const t = document.documentElement.getAttribute("data-theme");
  if (t === "dark" || t === "light") return t;
  // "auto": follow the OS preference.
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}
function updateThemeIcon() {
  // Show the icon for the mode you'll switch INTO: moon while light, sun while dark.
  $("#theme-icon").textContent = effectiveTheme() === "dark" ? "☀️" : "🌙";
}
(function initTheme() {
  const saved = localStorage.getItem("wxd_theme");
  if (saved) document.documentElement.setAttribute("data-theme", saved);
  updateThemeIcon();
  $("#theme-toggle").addEventListener("click", () => {
    const next = effectiveTheme() === "dark" ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", next);
    localStorage.setItem("wxd_theme", next);
    updateThemeIcon();
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
// Show only the credential group for the selected cloud provider.
function showCredsFor(provider) {
  for (const group of document.querySelectorAll("#creds-form .cred-group")) {
    group.hidden = group.dataset.provider !== provider;
  }
}
function selectedProvider() {
  const checked = document.querySelector('input[name="hyperscaler"]:checked');
  return checked ? checked.value : "aws";
}

// Provider id -> display name, filled by loadCatalog().
const PROVIDER_NAMES = {};

// Render the cluster-spec form from the selected provider's field schema.
async function renderProvisionSpec(provider) {
  const form = $("#provision-form");
  const title = $("#provision-title");
  const name = PROVIDER_NAMES[provider] || provider.toUpperCase();
  try {
    const fields = await api(`/catalog/provider-spec?provider=${encodeURIComponent(provider)}`);
    clear(form);
    if (!fields.length) {
      title.textContent = `Cluster spec (${name})`;
      form.appendChild(el("p", { class: "muted", text: `No provisioner for ${name} yet — coming soon.` }));
      return;
    }
    title.textContent = `Cluster spec (new ${name} cluster)`;
    // Spec fields with no default must be supplied (except known-optional ones).
    const OPTIONAL = new Set(["resource_tags", "ssh_key"]);
    for (const f of fields) {
      // Boolean fields (default "true"/"false") render as a checkbox.
      if (f.default === "true" || f.default === "false") {
        const cb = el("input", { attrs: { type: "checkbox", "data-provision-input": f.key } });
        cb.checked = f.default === "true";
        form.appendChild(el("label", { class: "check" }, [cb, el("span", { text: f.label })]));
        continue;
      }
      const required = f.default == null && !OPTIONAL.has(f.key);
      const attrs = { type: f.secret ? "password" : "text", "data-provision-input": f.key, autocomplete: "off" };
      if (required) attrs.required = "required";
      if (f.key === "cluster_name") {
        attrs.pattern = "[a-z0-9]([-a-z0-9.]*[a-z0-9])?";
        attrs.title = "Lowercase letters, numbers, '-' and '.'; must start and end with a letter or number.";
      }
      const input = el("input", { attrs });
      if (f.default != null) input.value = f.default;
      const labelText = el("span", { text: f.label });
      if (required) labelText.appendChild(el("span", { class: "req", text: " *" }));
      form.appendChild(el("label", {}, [labelText, input]));
    }
  } catch (e) {
    banner("fail", `Could not load cluster spec: ${e.message}`);
  }
}

async function loadCatalog() {
  try {
    const [hs, svcs] = await Promise.all([
      api("/catalog/hyperscalers"),
      api("/catalog/services"),
    ]);
    const hsList = $("#hyperscalers");
    clear(hsList);
    let firstEnabled = null;
    for (const h of hs) {
      PROVIDER_NAMES[h.id] = h.name;
      const radio = el("input", {
        attrs: { type: "radio", name: "hyperscaler", value: h.id },
      });
      if (!h.enabled) radio.disabled = true;
      if (h.enabled && firstEnabled === null) {
        firstEnabled = h.id;
        radio.checked = true;
      }
      radio.addEventListener("change", () => {
        showCredsFor(h.id);
        renderProvisionSpec(h.id);
      });
      const text = h.enabled ? h.name : `${h.name} (coming soon)`;
      const li = el("li", { class: "chip" + (h.enabled ? "" : " disabled") }, [
        el("label", {}, [radio, el("span", { text: text })]),
      ]);
      hsList.appendChild(li);
    }
    showCredsFor(firstEnabled || "aws");

    const svcList = $("#services");
    clear(svcList);
    for (const s of svcs) {
      const cb = el("input", {
        attrs: { type: "checkbox", "data-component": s.component },
      });
      if (s.default_selected) cb.checked = true;
      const label = el("label", { class: "check" }, [
        cb,
        el("span", { text: s.name }),
        el("span", { class: "muted comp", text: s.component }),
      ]);
      svcList.appendChild(el("li", {}, [label]));
    }
  } catch (e) {
    banner("fail", `Could not load catalog: ${e.message}`);
  }
}

// Comma-joined component tokens for the checked services.
function collectComponents() {
  const out = [];
  for (const cb of document.querySelectorAll('#services input[type="checkbox"]')) {
    if (cb.checked) out.push(cb.dataset.component);
  }
  return out.join(",");
}

// Show/hide form sections based on the chosen run mode.
function applyMode() {
  const existing = selectedMode() === "existing";
  $("#existing-panel").hidden = !existing;
  $("#provision-panel").hidden = existing;
  $("#provider-block").hidden = existing;
  if (existing) {
    // No cloud-provisioning creds needed; hide them all (IBM key stays).
    for (const g of document.querySelectorAll("#creds-form .cred-group")) g.hidden = true;
  } else {
    showCredsFor(selectedProvider());
    renderProvisionSpec(selectedProvider());
  }
}
for (const r of document.querySelectorAll('input[name="mode"]')) {
  r.addEventListener("change", applyMode);
}

// ---- prerequisites --------------------------------------------------------
function renderPrereqs(list) {
  const ul = $("#prereqs-list");
  clear(ul);
  let anyMissing = false;
  for (const t of list) {
    const ok = t.present;
    if (!ok) anyMissing = true;
    const badge = el("span", {
      class: "pr-badge " + (ok ? "ok" : t.installable ? "missing" : "warn"),
      text: ok ? "installed" : t.installable ? "missing" : "not found",
    });
    const name = el("span", { class: "pr-name", text: t.title });
    const detail = el("span", { class: "pr-detail muted", text: t.detail || (ok ? "" : t.installable ? "will be installed into ~/.wxd/bin" : "install manually") });
    ul.appendChild(el("li", { class: "pr-row" }, [badge, name, detail]));
  }
  $("#prereqs-install-btn").disabled = !anyMissing;
  // Collapse the panel to a single green line when nothing is missing.
  $("#prereqs-panel").classList.toggle("collapsed", !anyMissing);
  return anyMissing;
}

function prereqBanner(kind, msg) {
  const b = $("#prereqs-banner");
  b.hidden = false;
  b.className = `banner ${kind}`;
  b.textContent = msg;
}

async function loadPrereqs() {
  try {
    const list = await api("/prereqs");
    const missing = renderPrereqs(list);
    if (!missing) prereqBanner("ok", "All prerequisites are installed.");
    else $("#prereqs-banner").hidden = true;
  } catch (e) {
    prereqBanner("fail", `Could not check prerequisites: ${e.message}`);
  }
}

$("#prereqs-refresh-btn").addEventListener("click", async () => {
  const btn = $("#prereqs-refresh-btn");
  btn.disabled = true;
  prereqBanner("info", "Re-checking prerequisites…");
  try {
    const list = await api("/prereqs");
    const missing = renderPrereqs(list);
    prereqBanner(
      missing ? "info" : "ok",
      missing ? "Some prerequisites are missing — see the rows above." : "All prerequisites are installed."
    );
  } catch (e) {
    prereqBanner("fail", `Could not re-check: ${e.message}`);
  } finally {
    btn.disabled = false;
  }
});
$("#prereqs-install-btn").addEventListener("click", async () => {
  const btn = $("#prereqs-install-btn");
  btn.disabled = true;
  prereqBanner("info", "Installing missing prerequisites into ~/.wxd/bin … this can take a minute.");
  try {
    const list = await api("/prereqs/install", { method: "POST" });
    const missing = renderPrereqs(list);
    prereqBanner(missing ? "fail" : "ok", missing
      ? "Some prerequisites could not be installed — see the rows above."
      : "All prerequisites installed.");
  } catch (e) {
    prereqBanner("fail", `Install failed: ${e.message}`);
  }
});

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

// ---- resume a previous run ------------------------------------------------
// Re-attach the UI to an existing run: clear the log, reconnect the event
// stream (which replays the persisted live log), and re-render its state.
async function attachToRun(id) {
  if (!id) return;
  currentRunId = id;
  $("#log").textContent = "";
  connectEvents(id); // replays persisted events, incl. the live log
  await refreshRun();
  banner("info", `Re-attached to run ${id.slice(0, 8)} — live log restored below.`);
}

// Populate the "Resume a previous run" dropdown from the run store.
async function loadRuns() {
  const wrap = $("#resume-run");
  const sel = $("#runs-select");
  try {
    const runs = await api("/runs");
    if (!runs.length) {
      wrap.hidden = true;
      return;
    }
    clear(sel);
    // Newest first (the store lists by id; keep server order, newest appended).
    for (const r of [...runs].reverse()) {
      const label = `${r.id.slice(0, 8)} · ${r.mode} · ${r.status}`;
      sel.appendChild(el("option", { text: label, attrs: { value: r.id } }));
    }
    wrap.hidden = false;
  } catch {
    wrap.hidden = true; // listing is best-effort
  }
}

$("#resume-run-btn").addEventListener("click", () => {
  const id = $("#runs-select").value;
  if (id) attachToRun(id);
});
$("#runs-refresh-btn").addEventListener("click", loadRuns);

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

// Clear any prior/re-attached run from the view so a fresh start shows a clean
// slate (no stale progress, log, or controls leaking from the previous run).
function resetRunView() {
  if (eventSource) {
    eventSource.close();
    eventSource = null;
  }
  currentRunId = null;
  $("#run-meta").textContent = "No run yet.";
  clear($("#steps"));
  $("#log").textContent = "";
  $("#input-panel").hidden = true;
  $("#status-banner").hidden = true;
  for (const id of ["#pause-btn", "#resume-btn", "#retry-btn", "#destroy-btn"]) {
    $(id).disabled = true;
  }
}

$("#start-btn").addEventListener("click", async () => {
  try {
    // Starting afresh: drop any previously-attached run from the view first.
    resetRunView();
    const credentials = collectCredentials();
    // IBM entitlement key is required to install Software Hub / watsonx.data.
    if (!credentials.IBM_ENTITLEMENT_KEY) {
      banner("fail", "IBM entitlement key is required — enter it under Cloud credentials.");
      const field = document.querySelector('#creds-form input[data-cred="IBM_ENTITLEMENT_KEY"]');
      if (field) field.focus();
      return;
    }
    $("#log").textContent = "";
    const mode = selectedMode();
    const inputs = { components: collectComponents() };
    if (mode === "existing") {
      for (const i of document.querySelectorAll("#existing-form input[data-existing-input]")) {
        const v = i.value.trim();
        if (v) inputs[i.dataset.existingInput] = v;
      }
      for (const s of document.querySelectorAll("#existing-form input[data-existing-secret]")) {
        const v = s.value.trim();
        if (v) credentials[s.dataset.existingSecret] = v;
      }
    } else {
      // Provision mode: record the chosen cloud so the steps dispatch to the
      // right provisioner, then validate required spec fields natively.
      inputs.hyperscaler = selectedProvider();
      const pf = document.getElementById("provision-form");
      if (!pf.reportValidity()) {
        banner("fail", "Please complete the highlighted required cluster-spec field(s).");
        return;
      }
      for (const i of pf.querySelectorAll("input[data-provision-input]")) {
        if (i.type === "checkbox") {
          inputs[i.dataset.provisionInput] = i.checked ? "true" : "false";
          continue;
        }
        const v = i.value.trim();
        if (v) inputs[i.dataset.provisionInput] = v;
      }
    }
    const run = await api("/runs", {
      method: "POST",
      body: JSON.stringify({ mode, credentials, inputs }),
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
    loadRuns(); // make the new run available in the resume list
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
  // First confirmation: explicit, spells out what is destroyed.
  const proceed = window.confirm(
    "⚠️ DESTROY the provisioned OpenShift cluster?\n\n" +
      "This permanently deletes its cloud resources (EC2 instances, VPC, subnets, " +
      "NAT/ELB, EBS volumes, Route53 records). This cannot be undone."
  );
  if (!proceed) return;
  // Second confirmation: type the cluster name to match.
  let name = "destroy";
  try {
    const run = await api(`/runs/${currentRunId}`);
    name = (run.inputs && run.inputs.cluster_name) || "destroy";
  } catch {
    /* fall back to requiring the literal word "destroy" */
  }
  const typed = window.prompt(
    `Final confirmation — type the cluster name "${name}" to destroy it:`
  );
  if (typed === null) return; // cancelled
  if (typed.trim() !== name) {
    banner("fail", "Teardown cancelled — the name you typed did not match.");
    return;
  }
  banner("info", "Destroying cluster — watch the log for progress.");
  await api(`/runs/${currentRunId}/destroy`, { method: "POST" }).catch((e) =>
    banner("fail", `Could not start teardown: ${e.message}`)
  );
});

// ---- boot -----------------------------------------------------------------
loadPrereqs();
loadCatalog().then(applyMode);
applyMode();
loadRuns();
