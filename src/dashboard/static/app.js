const token = new URLSearchParams(window.location.search).get("token") || "";
const state = {
  embeddings: [],
  filter: "all",
  selected: null,
  activeRecallQuery: "",
  refreshing: false,
  lastEmbeddingRefresh: 0,
};

const LIVE_REFRESH_MS = 2000;
const EMBEDDING_REFRESH_MS = 5000;
const RECALL_REFRESH_MS = 5000;

const colorFor = {
  fact: "#4bc78c",
  decision: "#53bce8",
  task: "#b48cff",
};

const sourceLabels = {
  fact: "Facts",
  decision: "Decisions",
  task: "Tasks",
};

const exampleQueries = ["dashboard", "render", "recall", "task list", "hybrid scoring"];

const stopWords = new Set([
  "a",
  "an",
  "and",
  "are",
  "for",
  "how",
  "is",
  "it",
  "of",
  "on",
  "or",
  "the",
  "to",
  "what",
  "whats",
  "why",
  "with",
]);

function api(path, params = {}) {
  const url = new URL(path, window.location.origin);
  url.searchParams.set("token", token);
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== "") url.searchParams.set(key, value);
  }
  return fetch(url).then(async (response) => {
    if (!response.ok) {
      const body = await response.json().catch(() => ({}));
      throw new Error(body.error || response.statusText);
    }
    return response.json();
  });
}

function text(value, fallback = "") {
  return value === null || value === undefined || value === "" ? fallback : String(value);
}

function trimBody(value, max = 260) {
  const body = text(value, "No content recorded.");
  return body.length > max ? `${body.slice(0, max)}...` : body;
}

function setupTabs() {
  document.querySelectorAll(".tab").forEach((button) => {
    button.addEventListener("click", () => {
      document.querySelectorAll(".tab").forEach((tab) => tab.classList.remove("active"));
      document.querySelectorAll(".panel").forEach((panel) => panel.classList.remove("active"));
      button.classList.add("active");
      document.getElementById(button.dataset.panel).classList.add("active");
      if (button.dataset.panel === "map") drawMap();
    });
  });
}

function renderMetrics(counts) {
  const root = document.getElementById("overview-counts");
  root.innerHTML = "";
  for (const [label, count] of Object.entries(counts)) {
    const item = document.createElement("div");
    item.className = "metric";
    item.innerHTML = `<span>${label.replace("_", " ")}</span><b>${count}</b><small>${metricHint(label)}</small>`;
    root.appendChild(item);
  }
}

function metricHint(label) {
  const hints = {
    facts: "durable claims",
    decisions: "accepted rationale",
    tasks: "open and closed",
    writes_log: "audit events",
    embeddings: "indexed vectors",
    pending_writes: "review queue",
    commands: "verified commands",
  };
  return hints[label] || "records";
}

function renderBars(rootId, rows) {
  const root = document.getElementById(rootId);
  const max = Math.max(1, ...rows.map((row) => row.count));
  root.innerHTML = "";
  if (!rows.length) {
    root.textContent = "No rows.";
    return;
  }
  for (const row of rows) {
    const item = document.createElement("div");
    item.className = "bar";
    item.innerHTML = `
      <span>${row.label}</span>
      <span class="bar-track"><span class="bar-fill" style="width:${(row.count / max) * 100}%"></span></span>
      <span>${row.count}</span>
    `;
    root.appendChild(item);
  }
}

function renderActivity(payload) {
  renderBars("actor-bars", payload.by_actor);
  renderBars("table-bars", payload.by_table);
  const tbody = document.getElementById("activity-rows");
  tbody.innerHTML = "";
  for (const row of payload.writes) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td>${row.at}</td>
      <td>${row.actor}</td>
      <td>${row.table_name}${row.row_id ? ` #${row.row_id}` : ""}</td>
      <td>${row.action}</td>
      <td>${text(row.reason)}</td>
    `;
    tbody.appendChild(tr);
  }
}

function renderAudit(payload) {
  renderBars("source-bars", payload.source_counts);
  renderBars("confidence-bars", payload.confidence_histogram);
  renderBars(
    "coverage-bars",
    payload.embedding_coverage.map((row) => ({
      label: row.source_type,
      count: row.embedded,
    })),
  );
  const pending = payload.pending_writes.map((row) => `${row.label}: ${row.count}`).join(" | ");
  document.getElementById("audit-summary").textContent =
    `Stale facts: ${payload.stale_facts}. Pending writes: ${pending || "none"}.`;
}

function renderLegend() {
  const root = document.getElementById("map-legend");
  root.innerHTML = "";
  for (const key of ["fact", "decision", "task"]) {
    const item = document.createElement("span");
    item.className = "legend-item";
    item.innerHTML = `<i style="background:${colorFor[key]}"></i>${sourceLabels[key]}`;
    root.appendChild(item);
  }
}

function visiblePoints() {
  return state.embeddings.filter((point) => state.filter === "all" || point.source_type === state.filter);
}

function drawMap() {
  const canvas = document.getElementById("embedding-map");
  const ctx = canvas.getContext("2d");
  const points = visiblePoints();
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  const gradient = ctx.createLinearGradient(0, 0, canvas.width, canvas.height);
  gradient.addColorStop(0, "#101820");
  gradient.addColorStop(0.5, "#11131a");
  gradient.addColorStop(1, "#171224");
  ctx.fillStyle = gradient;
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  ctx.strokeStyle = "#252b35";
  ctx.lineWidth = 1;
  for (let x = 80; x < canvas.width; x += 120) {
    ctx.beginPath();
    ctx.moveTo(x, 0);
    ctx.lineTo(x, canvas.height);
    ctx.stroke();
  }
  for (let y = 70; y < canvas.height; y += 100) {
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(canvas.width, y);
    ctx.stroke();
  }
  ctx.fillStyle = "#788397";
  ctx.font = "14px ui-sans-serif, system-ui";
  ctx.fillText("PCA axis 1", canvas.width - 92, canvas.height - 18);
  ctx.save();
  ctx.translate(18, 86);
  ctx.rotate(-Math.PI / 2);
  ctx.fillText("PCA axis 2", 0, 0);
  ctx.restore();

  for (const point of points) {
    const px = ((point.x + 1) / 2) * (canvas.width - 90) + 45;
    const py = ((1 - point.y) / 2) * (canvas.height - 70) + 35;
    point._px = px;
    point._py = py;
    ctx.beginPath();
    ctx.fillStyle = `${colorFor[point.source_type] || "#e8bd55"}22`;
    ctx.arc(px, py, state.selected === point ? 18 : 12, 0, Math.PI * 2);
    ctx.fill();
    ctx.beginPath();
    ctx.fillStyle = colorFor[point.source_type] || "#e8bd55";
    ctx.arc(px, py, state.selected === point ? 9 : 6, 0, Math.PI * 2);
    ctx.fill();
  }
  const counts = countBy(points, "source_type");
  document.getElementById("map-meta").textContent =
    `${points.length} embedded rows | ${counts.fact || 0} facts | ${counts.decision || 0} decisions | ${counts.task || 0} tasks`;
}

function countBy(rows, key) {
  return rows.reduce((acc, row) => {
    acc[row[key]] = (acc[row[key]] || 0) + 1;
    return acc;
  }, {});
}

function setupMap() {
  const canvas = document.getElementById("embedding-map");
  document.getElementById("map-filter").addEventListener("change", (event) => {
    state.filter = event.target.value;
    state.selected = null;
    document.getElementById("point-detail").textContent = "Select a point.";
    drawMap();
  });
  canvas.addEventListener("click", (event) => {
    const rect = canvas.getBoundingClientRect();
    const x = ((event.clientX - rect.left) / rect.width) * canvas.width;
    const y = ((event.clientY - rect.top) / rect.height) * canvas.height;
    let best = null;
    let bestDist = 18;
    for (const point of visiblePoints()) {
      const dist = Math.hypot(point._px - x, point._py - y);
      if (dist < bestDist) {
        best = point;
        bestDist = dist;
      }
    }
    state.selected = best;
    const detail = document.getElementById("point-detail");
    if (best) {
      detail.innerHTML = `
        <strong>${best.title}</strong>
        <p><span class="badge">${best.source_type} #${best.source_id}</span><span class="badge">${text(best.source)}</span></p>
        <p>${trimBody(best.body, 420)}</p>
      `;
    }
    drawMap();
  });
}

function setupQueryChips() {
  const root = document.getElementById("query-chips");
  root.innerHTML = "";
  for (const query of exampleQueries) {
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = query;
    button.addEventListener("click", () => runRecall(query));
    root.appendChild(button);
  }
}

function setupRecall() {
  document.getElementById("recall-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    runRecall(document.getElementById("recall-query").value.trim());
  });
}

async function runRecall(q, options = {}) {
  const input = document.getElementById("recall-query");
  const summary = document.getElementById("recall-summary");
  const list = document.getElementById("recall-results");
  input.value = q;
  state.activeRecallQuery = q;
  if (!options.silent) {
    list.innerHTML = "";
  }
  if (!q) {
    summary.innerHTML = `<strong>Enter a query or use a chip.</strong> Core recall works best with focused project keywords.`;
    return;
  }
  if (!options.silent) {
    summary.textContent = "Running recall...";
  }
  try {
    let payload = await api("/api/recall", { q });
    let fallbackQuery = "";
    if (payload.returned_count === 0) {
      fallbackQuery = keywordFallback(q);
      if (fallbackQuery && fallbackQuery !== q) {
        payload = await api("/api/recall", { q: fallbackQuery });
      }
    }
    renderRecall(payload, fallbackQuery && fallbackQuery !== q ? q : "", options.silent);
  } catch (error) {
    summary.innerHTML = `<strong>Recall failed.</strong> ${error.message}`;
  }
}

function keywordFallback(query) {
  const tokens = query
    .toLowerCase()
    .replace(/[^a-z0-9_\-\s]/g, " ")
    .split(/\s+/)
    .filter((token) => token.length > 2 && !stopWords.has(token));
  return [...new Set(tokens)].slice(0, 4).join(" ");
}

function renderRecall(payload, originalQuery, refreshed = false) {
  const summary = document.getElementById("recall-summary");
  const list = document.getElementById("recall-results");
  const retry = originalQuery
    ? ` No exact bundle for "${originalQuery}", showing keyword retry "${payload.query}".`
    : "";
  summary.innerHTML =
    `<strong>${payload.returned_count}/${payload.candidate_count}</strong> returned in ${payload.elapsed_ms} ms via ${payload.mode}.${retry}${refreshed ? " Refreshed live." : ""}`;
  list.innerHTML = "";
  if (!payload.results.length) {
    const empty = document.createElement("li");
    empty.className = "empty-state";
    empty.textContent = "No durable rows matched. Try a shorter keyword query like dashboard, render, recall, or tasks.";
    list.appendChild(empty);
    return;
  }
  for (const hit of payload.results) {
    const li = document.createElement("li");
    li.className = `result ${hit.source_type}`;
    li.innerHTML = `
      <div class="result-head">
        <h3>${hit.rank}. ${hit.title}</h3>
        <span class="pill">${hit.source_type} #${hit.source_id}</span>
      </div>
      <div class="score-row">
        ${scoreMeter("final", hit.score)}
        ${scoreMeter("fts", hit.fts_score)}
        ${scoreMeter("vector", hit.vector_score)}
      </div>
      <p>${trimBody(hit.body, 520)}</p>
      <div class="muted-line">${text(hit.source, "source not recorded")} | ${hit.created_at}</div>
    `;
    list.appendChild(li);
  }
}

function scoreMeter(label, value) {
  const pct = Math.max(0, Math.min(1, value)) * 100;
  return `
    <span class="score-meter">
      <span>${label}</span>
      <i><b style="width:${pct}%"></b></i>
      <em>${value.toFixed(3)}</em>
    </span>
  `;
}

async function load() {
  setupTabs();
  setupMap();
  setupRecall();
  setupQueryChips();
  renderLegend();

  await refreshDashboard({ includeEmbeddings: true });
  setInterval(() => refreshDashboard(), LIVE_REFRESH_MS);
  setInterval(() => refreshActiveRecall(), RECALL_REFRESH_MS);
}

async function refreshDashboard(options = {}) {
  if (state.refreshing) {
    return;
  }
  state.refreshing = true;
  setLiveStatus("Refreshing");
  const includeEmbeddings =
    options.includeEmbeddings || Date.now() - state.lastEmbeddingRefresh >= EMBEDDING_REFRESH_MS;
  try {
    const requests = [
      api("/api/overview"),
      api("/api/activity"),
      api("/api/audit"),
    ];
    if (includeEmbeddings) {
      requests.push(api("/api/embeddings"));
    }
    const results = await Promise.allSettled(requests);
    if (results[0].status === "fulfilled") {
      renderOverview(results[0].value);
    }
    if (results[1].status === "fulfilled") {
      renderActivity(results[1].value);
    }
    if (results[2].status === "fulfilled") {
      renderAudit(results[2].value);
    }
    if (includeEmbeddings) {
      if (results[3].status === "fulfilled") {
        state.lastEmbeddingRefresh = Date.now();
        renderEmbeddings(results[3].value);
      } else {
        document.getElementById("map-meta").textContent = results[3].reason.message;
      }
    }
    setLiveStatus(`Live | ${new Date().toLocaleTimeString()}`);
  } catch (error) {
    setLiveStatus(`Live error: ${error.message}`);
  } finally {
    state.refreshing = false;
  }
}

function renderOverview(overview) {
  document.getElementById("project-meta").textContent =
    `${overview.project_name} | ${overview.retrieval_mode} | ${overview.repo_root}`;
  document.getElementById("latest-write-count").textContent = overview.recent_writes.length;
  document.getElementById("retrieval-mode").textContent = overview.retrieval_mode;
  document.getElementById("schema-version").textContent = overview.schema_version;
  renderMetrics(overview.counts);
  document.getElementById("state-body").textContent = text(overview.latest_state?.body, "No state narrative recorded.");
  document.getElementById("arch-body").textContent = text(overview.latest_arch?.body, "No architecture narrative recorded.");
}

function renderEmbeddings(payload) {
  const selectedKey = state.selected ? pointKey(state.selected) : "";
  state.embeddings = payload.points;
  if (selectedKey) {
    state.selected = state.embeddings.find((point) => pointKey(point) === selectedKey) || null;
  }
  drawMap();
}

function pointKey(point) {
  return `${point.source_type}:${point.source_id}`;
}

function refreshActiveRecall() {
  const activePanel = document.querySelector(".panel.active")?.id;
  if (activePanel === "recall" && state.activeRecallQuery) {
    runRecall(state.activeRecallQuery, { silent: true });
  }
}

function setLiveStatus(message) {
  document.getElementById("live-status").textContent = message;
}

load().catch((error) => {
  document.getElementById("project-meta").textContent = error.message;
});
