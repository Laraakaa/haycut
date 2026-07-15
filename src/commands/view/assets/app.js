// HayCut // View — vanilla JS, no build step, no dependencies.
// Fetches /api/runs and /api/runs/:id and renders the dashboard. Tasks whose
// status is still "open" are polled, laying the groundwork for watching a
// live agent run outside of evals.

const state = {
  runs: [],
  selectedId: null,
  pollTimer: null,
};

const POLL_MS = 2000;

document.addEventListener("DOMContentLoaded", () => {
  document.getElementById("refresh-btn").addEventListener("click", loadRunList);
  document.getElementById("tabs").addEventListener("click", onTabClick);
  loadRunList();
});

async function loadRunList() {
  try {
    const res = await fetch("/api/runs");
    const runs = await res.json();
    state.runs = runs;
    renderRunList(runs);

    if (state.selectedId && runs.some((run) => run.id === state.selectedId)) {
      return;
    }
    if (runs.length > 0) {
      selectRun(runs[0].id);
    }
  } catch (error) {
    document.getElementById("run-list").innerHTML =
      `<div class="empty-note">Failed to load runs: ${escapeHtml(String(error))}</div>`;
  }
}

function renderRunList(runs) {
  const list = document.getElementById("run-list");
  if (runs.length === 0) {
    list.innerHTML = '<div class="empty-note">No eval results or tasks found yet.</div>';
    return;
  }

  list.innerHTML = "";
  for (const run of runs) {
    const item = document.createElement("div");
    item.className = "run-item" + (run.id === state.selectedId ? " selected" : "");
    item.dataset.id = run.id;

    const tokens = run.total_model_tokens != null ? `${run.total_model_tokens} tok` : "";
    item.innerHTML = `
      <div class="run-item-top">
        <span class="run-item-title">${escapeHtml(run.title)}</span>
        <span class="badge ${badgeClass(run.status)}">${escapeHtml(run.status)}</span>
      </div>
      <div class="run-item-meta">
        <span>${escapeHtml(run.kind)}</span>
        <span>${escapeHtml(tokens)}</span>
      </div>
    `;
    item.addEventListener("click", () => selectRun(run.id));
    list.appendChild(item);
  }
}

async function selectRun(id) {
  state.selectedId = id;
  document.querySelectorAll(".run-item").forEach((el) => {
    el.classList.toggle("selected", el.dataset.id === id);
  });

  try {
    const res = await fetch(`/api/runs/${id}`);
    if (!res.ok) {
      throw new Error(`${res.status} ${res.statusText}`);
    }
    const detail = await res.json();
    renderDetail(detail);
    scheduleLivePoll(detail);
  } catch (error) {
    console.error("failed to load run detail", error);
  }
}

function scheduleLivePoll(detail) {
  if (state.pollTimer) {
    clearInterval(state.pollTimer);
    state.pollTimer = null;
  }

  const isLive = detail.kind === "task" && detail.status === "open";
  const dot = document.getElementById("live-indicator");
  dot.classList.toggle("live", isLive);
  dot.title = isLive ? "Watching live task" : "No live runs";

  if (isLive) {
    state.pollTimer = setInterval(async () => {
      try {
        const res = await fetch(`/api/runs/${detail.id}`);
        if (!res.ok) return;
        const fresh = await res.json();
        renderDetail(fresh);
        if (fresh.status !== "open") {
          clearInterval(state.pollTimer);
          state.pollTimer = null;
          dot.classList.remove("live");
          dot.title = "No live runs";
          loadRunList();
        }
      } catch (error) {
        console.error("poll failed", error);
      }
    }, POLL_MS);
  }
}

function renderDetail(detail) {
  document.getElementById("empty-state").classList.add("hidden");
  document.getElementById("run-detail").classList.remove("hidden");

  document.getElementById("detail-title").textContent = detail.title;
  const statusBadge = document.getElementById("detail-status");
  statusBadge.textContent = detail.status;
  statusBadge.className = `badge ${badgeClass(detail.status)}`;
  document.getElementById("detail-goal").textContent = detail.goal || "\u2014";
  document.getElementById("detail-verify").textContent = detail.verify || "\u2014";
  document.getElementById("detail-id").textContent = detail.id;

  renderStatCards(detail);
  renderWorkflowGraph(detail.workflow, primitiveIndex(detail));
  renderChecks(detail.checks);
  renderPatch(detail.patch_text);
  renderCalls(detail.steps);
  renderContext(detail);
  renderModelUsage(detail.model_usage);
  renderWorkflowSpec(detail);
  renderManifests(detail.manifests);
}

function renderStatCards(detail) {
  const ts = detail.token_summary;
  const cards = [
    ["Model input", ts.model_input_tokens],
    ["Model output", ts.model_output_tokens],
    ["Total model", ts.total_model_tokens],
    ["Packet input", ts.packet_input_tokens],
    ["Total context", ts.total_context_tokens],
    ["Budget used", `${detail.budget.packet_tokens_used} / ${detail.budget.hard_tokens}`],
  ];
  const row = document.getElementById("stat-cards");
  row.innerHTML = cards
    .map(
      ([label, value]) => `
      <div class="stat-card">
        <div class="stat-value">${escapeHtml(String(value))}</div>
        <div class="stat-label">${escapeHtml(label)}</div>
      </div>`
    )
    .join("");
}

// primitiveIndex looks up executor/phase for a primitive_id from the
// backend's static registry listing (RunDetail.primitives), replacing what
// used to be a hardcoded NodeOp::executor() mirror kept in sync by hand.
function primitiveIndex(detail) {
  const index = new Map();
  for (const primitive of detail.primitives || []) {
    index.set(primitive.id, primitive);
  }
  return index;
}

function renderWorkflowGraph(workflow, primitives) {
  const list = document.getElementById("workflow-graph");
  const nodes = workflow && workflow.nodes ? workflow.nodes : [];
  if (nodes.length === 0) {
    list.innerHTML = '<div class="empty-note">No workflow nodes recorded yet.</div>';
    return;
  }
  list.innerHTML = nodes
    .map((node) => {
      const deps = node.depends_on && node.depends_on.length > 0 ? node.depends_on.join(", ") : "—";
      const outcome = node.outcome ? escapeHtml(node.outcome) : "";
      const primitive = primitives.get(node.op);
      const executor = primitive ? primitive.executor : "deterministic";
      return `
      <li class="workflow-node status-${escapeHtml(node.status)} exec-${escapeHtml(executor)}">
        <div class="workflow-node-head">
          <span class="workflow-node-id">${escapeHtml(node.id)}</span>
          <span class="workflow-node-op">${escapeHtml(node.op.replace(/_/g, " "))}</span>
          <span class="exec-tag">${escapeHtml(executor.replace("_", " "))}</span>
          <span class="status-tag">${escapeHtml(node.status)}</span>
        </div>
        <div class="workflow-node-deps">depends on: ${escapeHtml(deps)}</div>
        ${outcome ? `<div class="workflow-node-outcome">${outcome}</div>` : ""}
      </li>`;
    })
    .join("");
}

function renderWorkflowSpec(detail) {
  const meta = document.getElementById("workflow-spec-meta");
  const list = document.getElementById("workflow-spec-graph");
  const spec = detail.workflow_spec;

  if (!spec) {
    meta.innerHTML = "";
    list.innerHTML =
      '<div class="empty-note">No compiled workflow spec recorded for this run.</div>';
    return;
  }

  meta.innerHTML = `
    <div class="workflow-spec-meta-row">
      <span>schema v${escapeHtml(String(spec.schema_version))}</span>
      <span>compiler ${escapeHtml(spec.compiler_version)}</span>
      <span>entrypoints: ${escapeHtml((spec.entrypoints || []).join(", ") || "—")}</span>
    </div>`;

  const primitives = primitiveIndex(detail);
  list.innerHTML = spec.nodes
    .map((node) => {
      const deps = node.dependencies.length > 0 ? node.dependencies.join(", ") : "—";
      const primitive = primitives.get(node.primitive_id);
      const executor = primitive ? primitive.executor : "deterministic";
      const guard = node.guard ? `<span class="status-tag">${escapeHtml(node.guard)}</span>` : "";
      return `
      <li class="workflow-node exec-${escapeHtml(executor)}">
        <div class="workflow-node-head">
          <span class="workflow-node-id">${escapeHtml(node.id)}</span>
          <span class="workflow-node-op">${escapeHtml(node.primitive_id)} v${escapeHtml(String(node.primitive_version))}</span>
          <span class="exec-tag">${escapeHtml(executor.replace("_", " "))}</span>
          ${guard}
        </div>
        <div class="workflow-node-deps">depends on: ${escapeHtml(deps)}</div>
      </li>`;
    })
    .join("");
}

function renderChecks(checks) {
  const block = document.getElementById("checks-block");
  const list = document.getElementById("checks-list");
  if (!checks || checks.length === 0) {
    block.classList.add("hidden");
    return;
  }
  block.classList.remove("hidden");
  list.innerHTML = checks
    .map(
      (check) => `
      <div class="check-row">
        <span class="badge ${badgeClass(check.verdict)}">${escapeHtml(check.verdict)}</span>
        <div>
          <div>${escapeHtml(check.name)}</div>
          ${
            check.reasons && check.reasons.length
              ? `<div class="check-reasons">${check.reasons.map(escapeHtml).join(" &middot; ")}</div>`
              : ""
          }
        </div>
      </div>`
    )
    .join("");
}

function renderPatch(patchText) {
  const block = document.getElementById("patch-block");
  if (!patchText) {
    block.classList.add("hidden");
    return;
  }
  block.classList.remove("hidden");
  document.getElementById("patch-text").textContent = patchText;
}

function renderCalls(steps) {
  const list = document.getElementById("calls-list");
  if (!steps || steps.length === 0) {
    list.innerHTML = '<div class="empty-note">No LLM calls recorded yet.</div>';
    return;
  }

  list.innerHTML = "";
  for (const step of steps) {
    const card = document.createElement("div");
    card.className = "call-card";

    const inTok = step.reported_input_tokens ?? step.estimated_input_tokens;
    const outTok = step.reported_output_tokens ?? step.estimated_output_tokens;
    const drift =
      step.input_estimation_ratio != null && Math.abs(step.input_estimation_ratio - 1) > 0.15
        ? `<span class="drift-warn">${step.input_estimation_ratio.toFixed(2)}x est</span>`
        : "";

    card.innerHTML = `
      <div class="call-head">
        <span class="call-chevron">&#9656;</span>
        <span class="call-index">#${step.step_index}</span>
        <span class="call-purpose">${escapeHtml(step.purpose)}</span>
        <span class="call-model">${escapeHtml(step.model)}</span>
        <span class="call-tokens">${inTok}in / ${outTok}out ${drift}</span>
      </div>
      <div class="call-body">
        <div class="call-section-label">Prompt</div>
        <pre class="code-block">${escapeHtml(step.prompt)}</pre>
        <div class="call-section-label">Response</div>
        <pre class="code-block">${escapeHtml(step.response)}</pre>
        <div class="call-section-label">Action</div>
        <pre class="code-block">${escapeHtml(step.action_json)}</pre>
        <div class="call-section-label">Observation</div>
        <pre class="code-block">${escapeHtml(step.observation)}</pre>
      </div>
    `;
    card.querySelector(".call-head").addEventListener("click", () => {
      card.classList.toggle("open");
    });
    list.appendChild(card);
  }
}

function renderContext(detail) {
  const ts = detail.token_summary;
  const max = Math.max(ts.packet_input_tokens, ts.model_input_tokens, ts.model_output_tokens, 1);
  const bars = [
    ["Packet (evidence)", ts.packet_input_tokens],
    ["Model input", ts.model_input_tokens],
    ["Model output", ts.model_output_tokens],
  ];
  document.getElementById("context-bars").innerHTML = bars
    .map(
      ([label, value]) => `
      <div class="bar-row">
        <span class="bar-label">${escapeHtml(label)}</span>
        <div class="bar-track"><div class="bar-fill" style="width:${(value / max) * 100}%"></div></div>
        <span class="bar-value">${value}</span>
      </div>`
    )
    .join("");

  const contextList = document.getElementById("available-context");
  const contexts = detail.available_context || [];
  if (contexts.length === 0) {
    contextList.innerHTML = '<div class="empty-note">No off-site context surfaced.</div>';
  } else {
    contextList.innerHTML = contexts
      .map((context) => {
        const relevant =
          context.relevant === true ? "relevant" : context.relevant === false ? "skipped" : "unjudged";
        return `
        <div class="context-item">
          <span class="badge ${badgeClass(relevant)}">${escapeHtml(relevant)}</span>
          <strong>${escapeHtml(context.symbol)}</strong>
          <span class="context-path">${escapeHtml(context.path)}</span>
        </div>`;
      })
      .join("");
  }

  const tbody = document.querySelector("#runs-table tbody");
  const runs = detail.runs || [];
  tbody.innerHTML = runs.length
    ? runs
        .map(
          (run) => `
        <tr>
          <td>${escapeHtml(run.command)}</td>
          <td>${run.exit_code ?? "\u2014"}</td>
          <td>${run.raw_tokens ?? "\u2014"}</td>
          <td>${run.packet_tokens ?? "\u2014"}</td>
        </tr>`
        )
        .join("")
    : '<tr><td colspan="4" class="empty-note">No captured runs.</td></tr>';
}

function renderUsageRow(row) {
  return `
      <div class="usage-row">
        <div>
          <div class="usage-model">${escapeHtml(row.model)}</div>
          <div class="usage-purpose">${escapeHtml(row.purpose)} &times; ${row.calls}</div>
        </div>
        <div class="usage-tokens">
          est ${row.estimated_input_tokens}in / ${row.estimated_output_tokens}out<br/>
          rep ${row.reported_input_tokens}in / ${row.reported_output_tokens}out
        </div>
        <div class="usage-tokens">
          ${row.input_estimation_ratio != null ? row.input_estimation_ratio.toFixed(2) + "x" : "\u2014"}
        </div>
      </div>`;
}

function renderUsageGroup(label, rows) {
  if (rows.length === 0) return "";
  const total = rows.reduce(
    (sum, row) => sum + row.reported_input_tokens + row.reported_output_tokens,
    0
  );
  return `
      <div class="usage-group">
        <div class="usage-group-heading">
          <span>${label}</span>
          <span class="usage-group-total">${total.toLocaleString()} tokens</span>
        </div>
        ${rows.map(renderUsageRow).join("")}
      </div>`;
}

function renderModelUsage(usage) {
  const container = document.getElementById("model-usage");
  if (!usage || usage.length === 0) {
    container.innerHTML = '<div class="empty-note">No model usage recorded yet.</div>';
    return;
  }
  const billed = usage.filter((row) => row.billed);
  const nonBilled = usage.filter((row) => !row.billed);
  container.innerHTML =
    renderUsageGroup("Billed", billed) + renderUsageGroup("Non-billed", nonBilled);
}

function renderManifests(manifests) {
  const list = document.getElementById("manifests-list");
  if (!manifests || manifests.length === 0) {
    list.innerHTML = '<div class="empty-note">No request manifests recorded yet.</div>';
    return;
  }

  list.innerHTML = "";
  for (const manifest of manifests) {
    const card = document.createElement("div");
    card.className = "call-card";

    const inTok = manifest.reported_input_tokens ?? manifest.estimated_input_tokens;
    const outTok = manifest.reported_output_tokens ?? manifest.estimated_output_tokens;
    const comparison = manifest.comparison;
    const verdictBadge = comparison
      ? `<span class="badge ${badgeClass(comparison.verdict.toLowerCase())}">${escapeHtml(comparison.verdict)}</span>`
      : "";

    card.innerHTML = `
      <div class="call-head">
        <span class="call-chevron">&#9656;</span>
        <span class="call-index">#${manifest.step_index}</span>
        <span class="call-purpose">${escapeHtml(manifest.primitive_id)} v${escapeHtml(String(manifest.primitive_version))} &middot; ${escapeHtml(manifest.phase)}</span>
        <span class="call-model">${escapeHtml(manifest.model)}</span>
        <span class="badge ${badgeClass(manifest.status)}">${escapeHtml(manifest.status)}</span>
        ${verdictBadge}
        <span class="call-tokens">${inTok}in / ${outTok}out</span>
      </div>
      <div class="call-body">
        <div class="call-section-label">Segments</div>
        ${renderManifestSegments(manifest.segments)}
        ${comparison ? renderManifestComparison(comparison) : ""}
        ${manifest.error_summary ? `<div class="call-section-label">Error</div><pre class="code-block">${escapeHtml(manifest.error_summary)}</pre>` : ""}
      </div>
    `;
    card.querySelector(".call-head").addEventListener("click", () => {
      card.classList.toggle("open");
    });
    list.appendChild(card);
  }
}

function renderManifestSegments(segments) {
  if (!segments || segments.length === 0) {
    return '<div class="empty-note">No segments recorded.</div>';
  }
  return `
    <table class="runs-table">
      <thead><tr><th>role</th><th>category</th><th>producer</th><th>digest</th><th>tokens</th></tr></thead>
      <tbody>
        ${segments
          .map(
            (segment) => `
          <tr>
            <td>${escapeHtml(segment.role)}</td>
            <td>${escapeHtml(segment.category)}</td>
            <td>${escapeHtml(segment.producer_id)} v${escapeHtml(String(segment.producer_version))}</td>
            <td>${escapeHtml(segment.content_digest.slice(0, 12))}</td>
            <td>${segment.estimated_tokens}</td>
          </tr>`
          )
          .join("")}
      </tbody>
    </table>`;
}

function renderManifestComparison(comparison) {
  const missing =
    comparison.required_categories_missing && comparison.required_categories_missing.length
      ? comparison.required_categories_missing.join(", ")
      : "none";
  const reasons =
    comparison.reasons && comparison.reasons.length
      ? comparison.reasons.map(escapeHtml).join(" &middot; ")
      : "—";
  return `
    <div class="call-section-label">Shadow-mode comparison</div>
    <div class="context-item">
      <span>legacy ${comparison.legacy_tokens} tok / compiled ${comparison.compiled_tokens} tok</span>
      <span>missing categories: ${escapeHtml(missing)}</span>
      <span>authoritative: ${comparison.authoritative}</span>
    </div>
    <div class="check-reasons">${reasons}</div>`;
}

function onTabClick(event) {
  const button = event.target.closest(".tab");
  if (!button) return;
  const tab = button.dataset.tab;

  document.querySelectorAll(".tab").forEach((el) => el.classList.toggle("active", el === button));
  document.querySelectorAll(".tab-panel").forEach((el) => {
    el.classList.toggle("active", el.id === `panel-${tab}`);
  });
}

function badgeClass(status) {
  switch (status) {
    case "pass":
    case "relevant":
      return "badge-pass";
    case "warn":
    case "unjudged":
      return "badge-warn";
    case "fail":
      return "badge-fail";
    case "open":
      return "badge-open";
    case "closed":
    case "skipped":
      return "badge-closed";
    default:
      return "badge-closed";
  }
}

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}
