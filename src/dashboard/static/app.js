const token = new URLSearchParams(window.location.search).get("token") || "";
const state = {
  embeddings: [],
  filter: "all",
  selected: null,
  activeRecallQuery: "",
  refreshing: false,
  lastEmbeddingRefresh: 0,
  metrics: null,
  uplot: null,
  metricsRange: "7d",
  seriesPayload: null,
  seriesPoints: [],
  seriesSig: "",
  seriesWindowDrawn: "",
  metricsZoomed: false,
  seriesInFlight: false,
};

const LIVE_REFRESH_MS = 2000;
const EMBEDDING_REFRESH_MS = 5000;
const RECALL_REFRESH_MS = 5000;

const colorFor = {
  fact: "#1fe88a",
  decision: "#19e3ff",
  task: "#a45cff",
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
      document.body.dataset.tab = button.dataset.panel;
      if (button.dataset.panel === "map") drawMap();
      if (button.dataset.panel === "metrics") refreshSeries({ refit: true });
    });
  });
}

function fmtNum(value) {
  const n = Number(value) || 0;
  return n.toLocaleString("en-US");
}

function periodEmpty(t) {
  return !t || (t.recalls === 0 && t.sessions === 0);
}

function offsetLabel(t) {
  return t.context_offset_pct === null || t.context_offset_pct === undefined
    ? "—"
    : `${t.context_offset_pct.toFixed(0)}%`;
}

function renderMetricsPanel(payload) {
  state.metrics = payload;
  const banner = document.getElementById("metrics-state");
  const body = document.getElementById("metrics-body");

  if (!payload.enabled) {
    banner.innerHTML =
      "<strong>Token accounting is off</strong> for this machine. Enable it (opt-in, per-machine) with <code>memhub metrics enable</code>.";
    body.classList.add("hidden");
    destroyChart();
    return;
  }

  const empty =
    periodEmpty(payload.totals_7d) &&
    periodEmpty(payload.totals_30d) &&
    payload.sessions.length === 0;
  if (empty) {
    banner.textContent = "Metrics enabled — no recall or session data captured yet.";
    body.classList.add("hidden");
    destroyChart();
    return;
  }

  const scrape = payload.last_scrape_ts ? ` · last scrape ${payload.last_scrape_ts}` : "";
  banner.innerHTML =
    `<strong>Token accounting on</strong> · recall proxy: ${payload.recall_proxy ? "on" : "off"}` +
    ` · session accounting: ${payload.session_accounting ? "on" : "off"}${scrape}`;
  body.classList.remove("hidden");
  renderMetricCards(payload);
  renderMetricsHelp(payload);
  renderSessionRows(payload.sessions);

  if (document.querySelector(".panel.active")?.id === "metrics") {
    refreshSeries();
  }
}

// Plain-English glossary for every card + chart series. Wording is
// bound by the proxy contract (CLAUDE.md): the offset is a
// counterfactual "vs full-ledger baseline", never "tokens saved", and
// tiktoken cl100k counts are ±10% of Anthropic's real tokenizer.
function renderMetricsHelp(payload) {
  const dl = document.getElementById("metrics-help");
  if (!dl || dl.dataset.filled === "1") return;
  const entries = [
    [
      "Context offset 7d / 30d",
      "Size of the recall bundle actually returned as a percentage of " +
        "loading the entire PROJECT_LEDGER.md instead. A counterfactual " +
        "baseline, not “tokens saved” — the agent would not necessarily " +
        "have loaded the whole ledger.",
    ],
    ["Recalls", "Number of memhub recall calls logged in the window."],
    [
      "Sessions",
      "Claude Code sessions scraped from the transcript JSONL in the window.",
    ],
    [
      "Real tokens",
      "Actual input + output tokens Claude Code burned, read from the " +
        "transcript (not memhub’s own writes).",
    ],
    [
      "With memhub (actual)",
      "Cumulative real token spend over time — per turn for the current " +
        "session, per session for the windowed ranges.",
    ],
    [
      "Without memhub (est.)",
      "The same curve plus, for each recall, the extra context it would " +
        "have cost to load the full ledger instead of the targeted " +
        "bundle. An estimate; it only diverges from actual where recall " +
        "logging exists.",
    ],
    [
      "Tokenizer caveat",
      "Counts use tiktoken cl100k, ±10% vs Anthropic’s real tokenizer. " +
        "Ratios are sound (same yardstick both sides); treat absolute " +
        "token counts as estimates.",
    ],
  ];
  dl.innerHTML = "";
  for (const [term, def] of entries) {
    const dt = document.createElement("dt");
    dt.textContent = term;
    const dd = document.createElement("dd");
    dd.textContent = def;
    dl.appendChild(dt);
    dl.appendChild(dd);
  }
  dl.dataset.filled = "1";
}

function renderMetricCards(payload) {
  const root = document.getElementById("metrics-cards");
  const t7 = payload.totals_7d;
  const t30 = payload.totals_30d;
  const cards = [
    {
      label: "Context offset 7d",
      value: offsetLabel(t7),
      hint: "of full-ledger baseline",
    },
    {
      label: "Context offset 30d",
      value: offsetLabel(t30),
      hint: "of full-ledger baseline",
    },
    { label: "Recalls 7d", value: fmtNum(t7.recalls), hint: "bundle vs ledger tokens" },
    { label: "Sessions 7d", value: fmtNum(t7.sessions), hint: "scraped transcripts" },
    {
      label: "Real tokens 7d",
      value: fmtNum(t7.input_tokens + t7.output_tokens),
      hint: `in ${fmtNum(t7.input_tokens)} · out ${fmtNum(t7.output_tokens)}`,
    },
  ];
  root.innerHTML = "";
  for (const card of cards) {
    const item = document.createElement("div");
    item.className = "metric";
    item.innerHTML = `<span>${card.label}</span><b>${card.value}</b><small>${card.hint}</small>`;
    root.appendChild(item);
  }
}

function renderSessionRows(sessions) {
  const tbody = document.getElementById("metrics-session-rows");
  tbody.innerHTML = "";
  if (!sessions.length) {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td colspan="6" class="muted-line">No sessions scraped yet.</td>`;
    tbody.appendChild(tr);
    return;
  }
  for (const s of sessions) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td>${s.session_id.slice(0, 8)}</td>
      <td>${text(s.agent)}</td>
      <td>${s.started_at.slice(0, 19)}</td>
      <td>${fmtNum(s.input_tokens)}</td>
      <td>${fmtNum(s.output_tokens)}</td>
      <td>${fmtNum(s.recall_calls)}</td>
    `;
    tbody.appendChild(tr);
  }
}

const ACTUAL_COLOR = "#3a86ff";
const COUNTER_COLOR = "#828b9f";

function destroyChart() {
  if (state.uplot) {
    state.uplot.destroy();
    state.uplot = null;
  }
  state.seriesWindowDrawn = "";
  hideChartTooltip();
}

function seriesSignature(p) {
  const n = p.points.length;
  const last = n ? p.points[n - 1] : null;
  return [
    p.window,
    p.granularity,
    n,
    last ? last.actual : 0,
    last ? last.counterfactual : 0,
    p.session_id || "",
  ].join(":");
}

// Pull /api/metrics/series for the selected range and (re)draw. The
// 2s dashboard poll calls this with no args (incremental); the range
// dropdown / tab-activate / Reset pass { refit:true } to rebuild and
// autoscale. A user-applied zoom is preserved across incremental
// updates and only cleared by Reset or double-click.
async function refreshSeries(opts = {}) {
  const active = document.querySelector(".panel.active")?.id === "metrics";
  if (!active && !opts.refit) return;
  if (state.seriesInFlight) return;
  state.seriesInFlight = true;
  try {
    const payload = await api("/api/metrics/series", {
      window: state.metricsRange,
    });
    state.seriesPayload = payload;
    drawSeriesChart(payload, opts);
  } catch (error) {
    const note = document.getElementById("metrics-series-note");
    if (note) note.textContent = `Chart unavailable: ${error.message}`;
  } finally {
    state.seriesInFlight = false;
  }
}

function buildSeriesData(points) {
  const xs = [];
  const actual = [];
  const counter = [];
  let lastX = -Infinity;
  const aligned = [];
  for (const p of points) {
    let x = Number(p.x) || 0;
    if (x <= lastX) x = lastX + 1; // uPlot needs strictly increasing x
    lastX = x;
    xs.push(x);
    actual.push(p.actual);
    counter.push(p.counterfactual);
    aligned.push(p);
  }
  state.seriesPoints = aligned;
  return [xs, actual, counter];
}

function chartTooltipPlugin() {
  const tip = document.getElementById("metrics-tooltip");
  return {
    hooks: {
      setCursor: (u) => {
        const idx = u.cursor.idx;
        if (idx == null || !tip) {
          hideChartTooltip();
          return;
        }
        const p = state.seriesPoints[idx];
        if (!p) {
          hideChartTooltip();
          return;
        }
        const rows = [
          `<strong>${p.label}</strong>`,
          `with memhub: <b>${fmtNum(p.actual)}</b>`,
          `without memhub (est.): <b>${fmtNum(p.counterfactual)}</b>`,
          `this point: ${fmtNum(p.delta)} tok`,
        ];
        if (p.recall_offset > 0) {
          rows.push(
            `est. ledger offset: ${fmtNum(p.recall_offset)} · recalls ${fmtNum(
              p.recalls,
            )}`,
          );
        }
        tip.innerHTML = rows.join("<br>");
        tip.classList.remove("hidden");
        const left = u.cursor.left ?? 0;
        const top = u.cursor.top ?? 0;
        const stage = tip.parentElement;
        const maxL = (stage ? stage.clientWidth : 0) - tip.offsetWidth - 12;
        tip.style.left = `${Math.max(8, Math.min(left + 16, maxL))}px`;
        tip.style.top = `${Math.max(8, top - 8)}px`;
      },
    },
  };
}

function hideChartTooltip() {
  const tip = document.getElementById("metrics-tooltip");
  if (tip) tip.classList.add("hidden");
}

function drawSeriesChart(payload, opts = {}) {
  const host = document.getElementById("metrics-burnup");
  const sub = document.getElementById("metrics-burnup-sub");
  const note = document.getElementById("metrics-series-note");
  if (!host) return;
  if (!payload || !payload.enabled) {
    destroyChart();
    return;
  }
  const width = host.clientWidth;
  if (width === 0) return; // panel hidden; redrawn on tab activate

  if (typeof uPlot === "undefined") {
    destroyChart();
    host.textContent = "Chart library failed to load.";
    return;
  }

  const points = payload.points || [];
  if (points.length === 0) {
    destroyChart();
    host.innerHTML = `<p class="empty-state">No ${
      payload.granularity === "turn" ? "turns in the current session" : "sessions in this range"
    } to chart yet.</p>`;
    if (sub) sub.textContent = "";
    if (note) note.textContent = "";
    return;
  }

  const data = buildSeriesData(points);
  const last = points[points.length - 1];
  if (sub) {
    const unit = payload.granularity === "turn" ? "turn" : "session";
    sub.textContent = `${points.length} ${unit}${
      points.length === 1 ? "" : "s"
    } · ${fmtNum(last.actual)} cumulative tokens`;
  }
  if (note) {
    note.textContent = payload.has_recall_signal
      ? `Dashed line: estimated context without memhub (full-ledger baseline) — ${fmtNum(
          last.counterfactual,
        )} est. vs ${fmtNum(last.actual)} actual. Estimate, ±10% tokenizer.`
      : "No recall offset logged in this range yet — the estimate line tracks actual until recalls are recorded.";
  }

  const sig = seriesSignature(payload);
  const windowChanged = state.seriesWindowDrawn !== payload.window;
  const rebuild = opts.refit || windowChanged || !state.uplot;

  if (!rebuild) {
    if (sig === state.seriesSig) return; // nothing new
    state.seriesSig = sig;
    // Preserve a user-applied zoom across the live poll; otherwise let
    // uPlot autoscale so the growing curve stays in frame.
    state.uplot.setData(data, !state.metricsZoomed);
    return;
  }

  destroyChart();
  host.innerHTML = "";
  state.metricsZoomed = false;
  state.seriesSig = sig;
  state.seriesWindowDrawn = payload.window;

  const axisStyle = {
    stroke: "#828b9f",
    grid: { stroke: "rgba(58, 134, 255, 0.10)", width: 1 },
    ticks: { stroke: "#262a36", width: 1 },
  };
  const chartOpts = {
    width,
    height: 320,
    cursor: { y: false, drag: { x: true, y: false } },
    legend: { show: true },
    scales: { x: { time: true } },
    plugins: [chartTooltipPlugin()],
    hooks: {
      setSelect: [
        (u) => {
          if (u.select.width > 0) state.metricsZoomed = true;
        },
      ],
    },
    axes: [
      axisStyle,
      {
        ...axisStyle,
        size: 70,
        values: (u, splits) => splits.map((v) => fmtNum(v)),
      },
    ],
    series: [
      {},
      {
        label: "with memhub (actual)",
        stroke: ACTUAL_COLOR,
        width: 2,
        fill: "rgba(58, 134, 255, 0.20)",
        points: { show: true, size: 6, stroke: ACTUAL_COLOR, fill: "#08090c", width: 2 },
        value: (u, v) => (v == null ? "—" : fmtNum(v)),
      },
      {
        label: "without memhub (est.)",
        stroke: COUNTER_COLOR,
        width: 2,
        dash: [6, 4],
        points: { show: false },
        value: (u, v) => (v == null ? "—" : fmtNum(v)),
      },
    ],
  };
  state.uplot = new uPlot(chartOpts, data, host);
}

function setupMetricsControls() {
  const select = document.getElementById("metrics-range");
  if (select) {
    select.value = state.metricsRange;
    select.addEventListener("change", (event) => {
      state.metricsRange = event.target.value;
      state.metricsZoomed = false;
      refreshSeries({ refit: true });
    });
  }
  const reset = document.getElementById("metrics-zoom-reset");
  if (reset) {
    reset.addEventListener("click", () => {
      state.metricsZoomed = false;
      if (state.seriesPayload) drawSeriesChart(state.seriesPayload, { refit: true });
    });
  }
  // uPlot resets scales on its own dblclick; just clear our flag so
  // the next poll doesn't re-pin the old zoom.
  const stage = document.getElementById("metrics-burnup");
  if (stage) {
    stage.addEventListener("dblclick", () => {
      state.metricsZoomed = false;
    });
  }
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

function renderRadialGauge(rootId, rows) {
  const root = document.getElementById(rootId);
  if (!root) return;
  const total = rows.reduce((acc, r) => acc + r.total, 0);
  const embedded = rows.reduce((acc, r) => acc + r.embedded, 0);
  const missing = rows.reduce((acc, r) => acc + r.missing, 0);
  const pct = total > 0 ? Math.round((embedded / total) * 100) : 0;
  const r = 52;
  const circ = 2 * Math.PI * r;
  const offset = circ * (1 - pct / 100);
  const legend = rows
    .map(
      (row) =>
        `<div><b>${row.embedded}</b>/${row.total} ${row.source_type}</div>`,
    )
    .join("");
  root.className = "coverage-gauge";
  root.innerHTML = `
    <div class="radial">
      <svg width="124" height="124" viewBox="0 0 124 124">
        <defs>
          <linearGradient id="garc-${rootId}" x1="0" y1="0" x2="1" y2="1">
            <stop offset="0" stop-color="#16b86d"/><stop offset="1" stop-color="#1fe88a"/>
          </linearGradient>
        </defs>
        <circle cx="62" cy="62" r="${r}" fill="none" stroke="#1c1f29" stroke-width="12"/>
        <circle cx="62" cy="62" r="${r}" fill="none" stroke="url(#garc-${rootId})"
          stroke-width="12" stroke-linecap="round"
          stroke-dasharray="${circ.toFixed(2)}" stroke-dashoffset="${offset.toFixed(2)}"/>
      </svg>
      <div class="num"><b>${pct}%</b><small>indexed</small></div>
    </div>
    <div class="leg">
      <div><b>${embedded}</b> rows embedded</div>
      <div><b>${missing}</b> awaiting index</div>
      ${legend}
    </div>
  `;
}

function renderAudit(payload) {
  renderBars("source-bars", payload.source_counts);
  renderRadialGauge("coverage-gauge", payload.embedding_coverage);
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
  gradient.addColorStop(0, "#0c0d12");
  gradient.addColorStop(0.5, "#0a0b0f");
  gradient.addColorStop(1, "#120d1c");
  ctx.fillStyle = gradient;
  ctx.fillRect(0, 0, canvas.width, canvas.height);

  ctx.strokeStyle = "rgba(164, 92, 255, 0.12)";
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
  ctx.fillStyle = "#828b9f";
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
  setupMetricsControls();
  renderLegend();

  await refreshDashboard({ includeEmbeddings: true });
  setInterval(() => refreshDashboard(), LIVE_REFRESH_MS);
  setInterval(() => refreshActiveRecall(), RECALL_REFRESH_MS);

  let resizeTimer = null;
  window.addEventListener("resize", () => {
    if (document.querySelector(".panel.active")?.id !== "metrics") return;
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => {
      if (state.seriesPayload) drawSeriesChart(state.seriesPayload, { refit: true });
    }, 150);
  });
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
      api("/api/metrics"),
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
    if (results[3].status === "fulfilled") {
      renderMetricsPanel(results[3].value);
    }
    if (includeEmbeddings) {
      if (results[4].status === "fulfilled") {
        state.lastEmbeddingRefresh = Date.now();
        renderEmbeddings(results[4].value);
      } else {
        document.getElementById("map-meta").textContent = results[4].reason.message;
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
