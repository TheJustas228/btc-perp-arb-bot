const state = {
  paused: false,
  pollMs: 1500,
  timer: null,
  lastPayloadAt: 0,
  thresholdDraft: null,
  capitalDraft: null,
  colorblindMode: localStorage.getItem("colorblindMode") === "1",
  collapsedSections: new Set(
    JSON.parse(localStorage.getItem("collapsedSections") || "[]")
  )
};

function focusedInputId() {
  const active = document.activeElement;
  return active?.tagName === "INPUT" ? active.id : null;
}

const els = {
  connectionDot: document.querySelector("#connectionDot"),
  statusText: document.querySelector("#statusText"),
  bestPrice: document.querySelector("#bestPrice"),
  bestExchange: document.querySelector("#bestExchange"),
  lowPrice: document.querySelector("#lowPrice"),
  lowExchange: document.querySelector("#lowExchange"),
  maxSpread: document.querySelector("#maxSpread"),
  maxSpreadPct: document.querySelector("#maxSpreadPct"),
  refreshRate: document.querySelector("#refreshRate"),
  tableWrap: document.querySelector("#tableWrap"),
  opportunitiesSection: document.querySelector("#opportunitiesSection"),
  opportunitiesWrap: document.querySelector("#opportunitiesWrap"),
  depthWrap: document.querySelector("#depthWrap"),
  recordedSection: document.querySelector("#recordedSection"),
  recordedWrap: document.querySelector("#recordedWrap"),
  spreadLogWrap: document.querySelector("#spreadLogWrap"),
  paperChecksWrap: document.querySelector("#paperChecksWrap"),
  fakeTradesWrap: document.querySelector("#fakeTradesWrap"),
  openPositionsWrap: document.querySelector("#openPositionsWrap"),
  closedTradesWrap: document.querySelector("#closedTradesWrap"),
  colorModeToggle: document.querySelector("#colorModeToggle"),
  pauseButton: document.querySelector("#pauseButton")
};

const usd = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  maximumFractionDigits: 2
});

const diffFormat = new Intl.NumberFormat("en-US", {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
  signDisplay: "always"
});

const plainNumber = new Intl.NumberFormat("en-US", {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2
});

function validPrices(prices) {
  return prices.filter((item) => item.ok && Number.isFinite(item.price));
}

function setConnection(status, text) {
  els.connectionDot.className = `dot ${status}`;
  els.statusText.textContent = text;
}

function applyColorMode() {
  document.body.classList.toggle("colorblindMode", state.colorblindMode);
  els.colorModeToggle.checked = state.colorblindMode;
}

function sectionKey(section, index) {
  if (section.id) return section.id;
  const title = section.querySelector("h2")?.textContent?.trim();
  if (title) return title.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  return `workspace-${index}`;
}

function persistCollapsedSections() {
  localStorage.setItem(
    "collapsedSections",
    JSON.stringify([...state.collapsedSections])
  );
}

function setSectionCollapsed(section, key, collapsed, button) {
  section.classList.toggle("workspaceCollapsed", collapsed);
  button.textContent = collapsed ? "▸" : "▾";
  button.setAttribute("aria-expanded", collapsed ? "false" : "true");
  button.setAttribute(
    "aria-label",
    `${collapsed ? "Expand" : "Collapse"} ${section.querySelector("h2")?.textContent || "section"}`
  );
  if (collapsed) {
    state.collapsedSections.add(key);
  } else {
    state.collapsedSections.delete(key);
  }
}

function initSectionToggles() {
  const sections = [...document.querySelectorAll(".workspace")];
  sections.forEach((section, index) => {
    const header = section.querySelector(".tableHeader");
    const wrap = section.querySelector(".tableWrap");
    if (!header || !wrap) return;

    const key = sectionKey(section, index);
    const toggle = document.createElement("button");
    toggle.type = "button";
    toggle.className = "sectionToggle";
    toggle.dataset.sectionKey = key;

    setSectionCollapsed(
      section,
      key,
      state.collapsedSections.has(key),
      toggle
    );

    toggle.addEventListener("click", () => {
      const next = !section.classList.contains("workspaceCollapsed");
      setSectionCollapsed(section, key, next, toggle);
      persistCollapsedSections();
    });

    header.append(toggle);
  });
}

function formatAge(iso) {
  if (!iso) return "never";
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(iso)) / 1000));
  if (seconds < 2) return "just now";
  return `${seconds}s ago`;
}

function renderSummary(payload) {
  const live = validPrices(payload.prices);
  els.refreshRate.textContent = `${payload.pollMs}ms`;

  if (live.length === 0) {
    els.bestPrice.textContent = "--";
    els.bestExchange.textContent = "No live exchanges";
    els.lowPrice.textContent = "--";
    els.lowExchange.textContent = "No live exchanges";
    els.maxSpread.textContent = "--";
    els.maxSpreadPct.textContent = "--";
    return;
  }

  const highest = [...live].sort((a, b) => b.price - a.price)[0];
  const lowest = [...live].sort((a, b) => a.price - b.price)[0];
  const spread = highest.price - lowest.price;
  const spreadPct = lowest.price > 0 ? (spread / lowest.price) * 100 : 0;

  els.bestPrice.textContent = usd.format(highest.price);
  els.bestExchange.textContent = highest.name;
  els.lowPrice.textContent = usd.format(lowest.price);
  els.lowExchange.textContent = lowest.name;
  els.maxSpread.textContent = diffFormat.format(spread);
  els.maxSpreadPct.textContent = `${spreadPct.toFixed(4)}%`;
}

function cellClass(diff) {
  if (!Number.isFinite(diff) || Math.abs(diff) < 0.005) return "neutral";
  return diff > 0 ? "positive" : "negative";
}

function renderTable(payload) {
  const prices = payload.prices;
  const headerCells = prices
    .map((exchange) => `<th title="${exchange.name}">${exchange.name}</th>`)
    .join("");

  const rows = prices
    .map((row) => {
      const staleClass = row.ok ? "" : " stale";
      const priceText = Number.isFinite(row.price) ? usd.format(row.price) : "--";
      const label = `
        <td class="${staleClass}">
          <div class="exchangeName">
            <strong>${row.name}</strong>
            <span>${priceText}</span>
          </div>
        </td>`;

      const cells = prices
        .map((column) => {
          if (row.id === column.id) {
            return `<td class="diff neutral">0.00</td>`;
          }

          const diff =
            Number.isFinite(row.price) && Number.isFinite(column.price)
              ? row.price - column.price
              : null;

          if (diff === null) {
            return `<td class="diff neutral">--</td>`;
          }

          return `<td class="diff ${cellClass(diff)}">${diffFormat.format(diff)}</td>`;
        })
        .join("");

      return `<tr>${label}${cells}</tr>`;
    })
    .join("");

  els.tableWrap.innerHTML = `
    <table class="matrixTable">
      <colgroup>
        <col class="exchangeColumn" />
        ${prices.map(() => "<col />").join("")}
      </colgroup>
      <thead>
        <tr>
          <th>Exchange / price</th>
          ${headerCells}
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function formatTimestamp(iso) {
  if (!iso) return "--";
  return new Intl.DateTimeFormat("en-US", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit"
  }).format(new Date(iso));
}

function formatDuration(iso) {
  if (!iso) return "--";
  const totalSeconds = Math.max(0, Math.floor((Date.now() - Date.parse(iso)) / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes < 60) return `${minutes}m ${seconds}s`;
  const hours = Math.floor(minutes / 60);
  return `${hours}h ${minutes % 60}m`;
}

function renderOpportunities(payload) {
  const opportunities = payload.opportunities || [];

  if (opportunities.length === 0) {
    els.opportunitiesWrap.innerHTML = `
      <table class="opportunitiesTable">
        <thead>
          <tr>
            <th>Buy exchange</th>
            <th>Sell exchange</th>
            <th>Buy price</th>
            <th>Sell price</th>
            <th>Spread / BTC</th>
            <th>Gross %</th>
            <th>Fees / BTC</th>
            <th>Net / BTC</th>
            <th>Status</th>
            <th>Detected</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
            <td class="diff neutral"></td>
          </tr>
        </tbody>
      </table>`;
    return;
  }

  const rows = opportunities
    .map((opportunity) => {
      const netClass = opportunity.estimatedNetProfit > 0 ? "positive" : "negative";

      return `
        <tr>
          <td>${opportunity.buyExchange}</td>
          <td>${opportunity.sellExchange}</td>
          <td>${usd.format(opportunity.buyPrice)}</td>
          <td>${usd.format(opportunity.sellPrice)}</td>
          <td>${diffFormat.format(opportunity.grossDifference)}</td>
          <td>${plainNumber.format(opportunity.grossPercentageDifference)}%</td>
          <td>${usd.format(opportunity.estimatedFees)}</td>
          <td class="diff ${netClass}">${diffFormat.format(opportunity.estimatedNetProfit)}</td>
          <td>${opportunity.status}</td>
          <td>${formatTimestamp(opportunity.detectedAt)}</td>
        </tr>`;
    })
    .join("");

  els.opportunitiesWrap.innerHTML = `
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Buy price</th>
          <th>Sell price</th>
          <th>Spread / BTC</th>
          <th>Gross %</th>
          <th>Fees / BTC</th>
          <th>Net / BTC</th>
          <th>Status</th>
          <th>Detected</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function renderRecordedOpportunities(payload) {
  const recorded = payload.recordedOpportunities || [];
  els.recordedSection.hidden = recorded.length === 0;

  if (recorded.length === 0) {
    els.recordedWrap.innerHTML = "";
    return;
  }

  const rows = recorded
    .map(
      (opportunity) => `
        <tr>
          <td>${opportunity.buyExchange}</td>
          <td>${opportunity.sellExchange}</td>
          <td>${Number.isFinite(opportunity.sizeBtc) ? `${plainNumber.format(opportunity.sizeBtc)} BTC` : ""}</td>
          <td>${usd.format(opportunity.buyPrice)}</td>
          <td>${usd.format(opportunity.sellPrice)}</td>
          <td>${diffFormat.format(opportunity.grossDifference)}</td>
          <td>${plainNumber.format(opportunity.grossPercentageDifference)}%</td>
          <td>${usd.format(opportunity.estimatedFees)}</td>
          <td class="diff positive">${diffFormat.format(opportunity.estimatedNetProfit)}</td>
          <td>${opportunity.status}</td>
          <td>${formatTimestamp(opportunity.detectedAt)}</td>
        </tr>`
    )
    .join("");

  els.recordedWrap.innerHTML = `
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Size</th>
          <th>Buy price</th>
          <th>Sell price</th>
          <th>Gross diff</th>
          <th>Gross %</th>
          <th>Est. fees</th>
          <th>Est. net</th>
          <th>Status</th>
          <th>Detected</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function renderDepthSimulations(payload) {
  if (focusedInputId() === "minDepthThreshold") return;
  const hasArbitrageOpportunity = (payload.opportunities || []).length > 0;
  const profitableScenarios = hasArbitrageOpportunity
    ? payload.maxProfitableScenarios || []
    : [];

  const renderDepthRow = (scenario, label) => {
    const netClass = scenario.estimatedNetProfit > 0 ? "positive" : "negative";
    const grossClass = scenario.grossProfit > 0 ? "positive" : "negative";

    return `
      <tr>
        <td>${label}</td>
        <td>${plainNumber.format(scenario.sizeBtc)} BTC</td>
        <td>${scenario.buyExchange}</td>
        <td>${scenario.sellExchange}</td>
        <td>${usd.format(scenario.buyAveragePrice)}</td>
        <td>${usd.format(scenario.sellAveragePrice)}</td>
        <td class="diff ${grossClass}">${diffFormat.format(scenario.grossDifference)}</td>
        <td class="diff ${grossClass}">${diffFormat.format(scenario.grossProfit)}</td>
        <td>${usd.format(scenario.estimatedFees)}</td>
        <td class="diff ${netClass}">${diffFormat.format(scenario.estimatedNetProfit)}</td>
        <td>${scenario.status}</td>
      </tr>`;
  };

  const rows = profitableScenarios.length > 0
    ? profitableScenarios
        .map((scenario, index) =>
          renderDepthRow(scenario, index === 0 ? "Best route" : `Alt route #${index + 1}`)
        )
        .join("")
    : `
      <tr>
        <td>Max profitable</td>
        <td>0.00 BTC</td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
        <td class="diff neutral"></td>
      </tr>`;
  const totalEstimatedNet = payload.cumulativeDepthNet || 0;
  const thresholdValue =
    state.thresholdDraft ?? Number(payload.minDepthNetThreshold || 0).toFixed(2);
  const sizing = payload.accountSizing || {};
  const capitalText = Number.isFinite(sizing.maxSizeBtc)
    ? `${plainNumber.format(sizing.maxSizeBtc)} BTC cap`
    : "--";
  const marginText =
    Number.isFinite(sizing.capitalUsd) && Number.isFinite(sizing.leverage)
      ? `${usd.format(sizing.capitalUsd)} at ${plainNumber.format(sizing.leverage)}x`
      : "";
  const capitalUsd = Number(sizing.capitalUsd || 0);
  const recommendedThreshold = Math.max(0.05, Math.min(1, capitalUsd * 0.002));
  const recommendationText = `${usd.format(recommendedThreshold)} recommended`;

  els.depthWrap.innerHTML = `
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Mode</th>
          <th>Size</th>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Avg buy</th>
          <th>Avg sell</th>
          <th>Gross / BTC</th>
          <th>Gross profit</th>
          <th>Est. fees</th>
          <th>Est. net</th>
          <th>Status</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>
    <div class="netTotal">
      <div>
        <span>Total est. net</span>
        <strong>${totalEstimatedNet > 0 ? diffFormat.format(totalEstimatedNet) : "--"}</strong>
        <span>${capitalText}</span>
        <span>${marginText}</span>
      </div>
      <div class="netControls">
        <label>
          Min executable net
          <input id="minDepthThreshold" type="number" min="0" step="0.01" value="${thresholdValue}">
        </label>
        <span class="controlHint">${recommendationText}</span>
        <button id="saveDepthSettings" type="button">Apply</button>
        <button id="resetDepthTotal" type="button">Reset</button>
      </div>
    </div>`;
}

async function saveDepthSettings() {
  const input = document.querySelector("#minDepthThreshold");
  const value = Math.max(0, Number(input?.value || 0));
  await fetch("/api/settings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ minDepthNetThreshold: value })
  });
  state.thresholdDraft = null;
  await loadTickers();
}

async function saveCapitalSettings() {
  const input = document.querySelector("#accountCapitalInput");
  const value = Math.max(1, Number(input?.value || 0));
  await fetch("/api/settings", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ accountCapitalUsd: value })
  });
  state.capitalDraft = null;
  await loadTickers();
}

async function resetDepthTotal() {
  await fetch("/api/reset-depth-total", { method: "POST" });
  await loadTickers();
}

async function resetFakeTrades() {
  await fetch("/api/reset-fake-trades", { method: "POST" });
  await loadTickers();
}

function renderSpreadLog(payload) {
  const records = [
    { label: "Max recorded spread", record: payload.maxSpreadRecord },
    { label: "Highest est. net", record: payload.highestNetRecord }
  ].filter((item) => item.record);

  if (records.length === 0) {
    els.spreadLogWrap.innerHTML = `
      <table class="opportunitiesTable">
        <thead>
          <tr>
            <th>Record</th>
            <th>Buy exchange</th>
            <th>Sell exchange</th>
            <th>Buy price</th>
            <th>Sell price</th>
            <th>Max spread</th>
            <th>Gross %</th>
            <th>Est. fees</th>
            <th>Est. net</th>
            <th>Status</th>
            <th>Detected</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td class="diff neutral" colspan="11">No interesting opportunity has been recorded.</td>
          </tr>
        </tbody>
      </table>`;
    return;
  }

  const rows = records
    .map(({ label, record }) => {
      const netClass = record.estimatedNetProfit > 0 ? "positive" : "negative";
      const spreadClass = record.grossDifference > 0 ? "positive" : "negative";

      return `
        <tr>
          <td>${label}</td>
          <td>${record.buyExchange}</td>
          <td>${record.sellExchange}</td>
          <td>${usd.format(record.buyPrice)}</td>
          <td>${usd.format(record.sellPrice)}</td>
          <td class="diff ${spreadClass}">${diffFormat.format(record.grossDifference)}</td>
          <td>${plainNumber.format(record.grossPercentageDifference)}%</td>
          <td>${usd.format(record.estimatedFees)}</td>
          <td class="diff ${netClass}">${diffFormat.format(record.estimatedNetProfit)}</td>
          <td>${record.status}</td>
          <td>${formatTimestamp(record.detectedAt)}</td>
        </tr>`;
    })
    .join("");

  els.spreadLogWrap.innerHTML = `
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Record</th>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Buy price</th>
          <th>Sell price</th>
          <th>Max spread</th>
          <th>Gross %</th>
          <th>Est. fees</th>
          <th>Est. net</th>
          <th>Status</th>
          <th>Detected</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function renderPaperChecks(payload) {
  const checks = payload.paperChecks || [];
  const survived = checks.filter((check) => check.survived).length;
  const survivalText =
    checks.length > 0
      ? `${survived}/${checks.length} survived executable threshold`
      : "No completed checks";

  if (checks.length === 0) {
    els.paperChecksWrap.innerHTML = `
      <div class="netTotal paperSummary">
        <div>
          <span>Survival</span>
          <strong>--</strong>
          <span>${survivalText}</span>
        </div>
      </div>
      <table class="opportunitiesTable">
        <thead>
          <tr>
            <th>Buy exchange</th>
            <th>Sell exchange</th>
            <th>Size</th>
            <th>Delay</th>
            <th>Initial net</th>
            <th>Recheck net</th>
            <th>Net change</th>
            <th>Status</th>
            <th>Checked</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td class="diff neutral" colspan="9">No paper execution check has completed.</td>
          </tr>
        </tbody>
      </table>`;
    return;
  }

  const rows = checks
    .map((check) => {
      const netClass = check.recheckNet > 0 ? "positive" : "negative";
      const changeClass = check.netChange > 0 ? "positive" : "negative";

      return `
        <tr>
          <td>${check.buyExchange}</td>
          <td>${check.sellExchange}</td>
          <td>${plainNumber.format(check.sizeBtc)} BTC</td>
          <td>${check.delayMs}ms</td>
          <td>${diffFormat.format(check.initialNet)}</td>
          <td class="diff ${netClass}">${Number.isFinite(check.recheckNet) ? diffFormat.format(check.recheckNet) : ""}</td>
          <td class="diff ${changeClass}">${Number.isFinite(check.netChange) ? diffFormat.format(check.netChange) : ""}</td>
          <td>${check.status}</td>
          <td>${formatTimestamp(check.checkedAt)}</td>
        </tr>`;
    })
    .join("");

  els.paperChecksWrap.innerHTML = `
    <div class="netTotal paperSummary">
      <div>
        <span>Survival</span>
        <strong>${plainNumber.format((survived / checks.length) * 100)}%</strong>
        <span>${survivalText}</span>
      </div>
    </div>
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Size</th>
          <th>Delay</th>
          <th>Initial net</th>
          <th>Recheck net</th>
          <th>Net change</th>
          <th>Status</th>
          <th>Checked</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function renderFakeTrades(payload) {
  if (focusedInputId() === "accountCapitalInput") return;
  const trades = payload.fakeTrades || [];
  const totalNet = Number(payload.fakeCumulativeNet || 0);
  const sizing = payload.accountSizing || {};
  const capitalValue =
    state.capitalDraft ?? Number(sizing.capitalUsd || 587).toFixed(2);
  const resetAge = formatDuration(payload.fakeTradesResetAt);

  if (trades.length === 0) {
    els.fakeTradesWrap.innerHTML = `
      <div class="netTotal paperSummary">
        <div>
          <span>Fake P/L</span>
          <strong>--</strong>
          <span>No fake trades executed</span>
          <span>Elapsed ${resetAge}</span>
        </div>
        <div class="netControls">
          <label>
            Total capital
            <input id="accountCapitalInput" type="number" min="1" step="1" value="${capitalValue}">
          </label>
          <button id="saveCapitalSettings" type="button">Apply</button>
          <button id="resetFakeTrades" type="button">Reset</button>
        </div>
      </div>
      <table class="opportunitiesTable">
        <thead>
          <tr>
            <th>Buy exchange</th>
            <th>Sell exchange</th>
            <th>Size</th>
            <th>Avg buy</th>
            <th>Avg sell</th>
            <th>Margin used</th>
            <th>Est. fees</th>
            <th>Est. net</th>
            <th>Status</th>
            <th>Time</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td class="diff neutral" colspan="10">No executable fake trade has been recorded.</td>
          </tr>
        </tbody>
      </table>`;
    return;
  }

  const rows = trades
    .map((trade) => {
      const netClass = trade.estimatedNet > 0 ? "positive" : "negative";
      return `
        <tr>
          <td>${trade.buyExchange}</td>
          <td>${trade.sellExchange}</td>
          <td>${plainNumber.format(trade.sizeBtc)} BTC</td>
          <td>${usd.format(trade.buyAveragePrice)}</td>
          <td>${usd.format(trade.sellAveragePrice)}</td>
          <td>${usd.format(trade.marginUsed)}</td>
          <td>${usd.format(trade.estimatedFees)}</td>
          <td class="diff ${netClass}">${diffFormat.format(trade.estimatedNet)}</td>
          <td>${trade.status}</td>
          <td>${formatTimestamp(trade.createdAt)}</td>
        </tr>`;
    })
    .join("");

  els.fakeTradesWrap.innerHTML = `
    <div class="netTotal paperSummary">
      <div>
        <span>Fake P/L</span>
        <strong>${totalNet > 0 ? diffFormat.format(totalNet) : "--"}</strong>
        <span>${trades.length} simulated trades</span>
        <span>Elapsed ${resetAge}</span>
      </div>
      <div class="netControls">
        <label>
          Total capital
          <input id="accountCapitalInput" type="number" min="1" step="1" value="${capitalValue}">
        </label>
        <button id="saveCapitalSettings" type="button">Apply</button>
        <button id="resetFakeTrades" type="button">Reset</button>
      </div>
    </div>
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Size</th>
          <th>Avg buy</th>
          <th>Avg sell</th>
          <th>Margin used</th>
          <th>Est. fees</th>
          <th>Est. net</th>
          <th>Status</th>
          <th>Time</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function renderOpenPositions(payload) {
  const positions = payload.openPositions || [];
  const stats = payload.openDecisionStats || {};
  const attempted = Number(stats.attempted || 0);
  const opened = Number(stats.opened || 0);
  const rejected = Math.max(0, attempted - opened);
  const topReasons = [
    ["No depth", Number(stats.rejectNoDepth || 0)],
    ["Basis not warm", Number(stats.rejectBasisNotWarm || 0)],
    ["Below basis", Number(stats.rejectBasisThreshold || 0)],
    ["Below shock", Number(stats.rejectShockThreshold || 0)],
    ["Fee multiple", Number(stats.rejectFeeMultiple || 0)],
    ["Haircut gate", Number(stats.rejectHaircut || 0)],
    ["Route quality avg", Number(stats.rejectRouteQualityAvg || 0)],
    ["Route quality ratio", Number(stats.rejectRouteQualityRealization || 0)],
    ["Pair already open", Number(stats.rejectPairAlreadyOpen || 0)],
    ["Max open", Number(stats.rejectMaxOpenPositions || 0)],
    ["Below min entry net", Number(stats.rejectMinEntryEstimatedNet || 0)],
    ["Not on route allowlist", Number(stats.rejectRouteAllowlist || 0)],
    ["Entry fees too high", Number(stats.rejectEntryFeesCap || 0)]
  ]
    .filter(([, count]) => count > 0)
    .sort((a, b) => b[1] - a[1])
    .slice(0, 5);
  const reasonsText =
    topReasons.length > 0
      ? topReasons
          .map(([label, count]) => `${label}: ${count}`)
          .join(" | ")
      : "No rejections yet";
  const decisionSummary = `
    <div class="netTotal paperSummary">
      <div>
        <span>Open decisions</span>
        <strong>${attempted === 0 ? "--" : `${opened}/${attempted}`}</strong>
        <span>${opened} opened, ${rejected} rejected</span>
        <span>${reasonsText}</span>
      </div>
    </div>`;
 
  if (positions.length === 0) {
    els.openPositionsWrap.innerHTML = `
      ${decisionSummary}
      <table class="opportunitiesTable">
        <thead>
          <tr>
            <th>Buy exchange</th>
            <th>Sell exchange</th>
            <th>Size</th>
            <th>Held</th>
            <th>Entry spread</th>
            <th>Trigger spread</th>
            <th>Exit spread</th>
            <th>Unrealized net</th>
            <th>Opened</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td class="diff neutral" colspan="9">No open simulated positions.</td>
          </tr>
        </tbody>
      </table>`;
    return;
  }

  const rows = positions
    .map((position) => {
      const entrySpread = position.entrySellPrice - position.entryBuyPrice;
      const unrealizedClass =
        Number.isFinite(position.currentUnrealizedNet) && position.currentUnrealizedNet > 0
          ? "positive"
          : "negative";
      const triggerClass =
        Number.isFinite(position.currentTriggerSpread) && position.currentTriggerSpread > 0
          ? "positive"
          : "negative";

      return `
        <tr>
          <td>${position.buyExchange}</td>
          <td>${position.sellExchange}</td>
          <td>${plainNumber.format(position.sizeBtc)} BTC</td>
          <td>${position.ticksHeld} ticks</td>
          <td class="diff positive">${diffFormat.format(entrySpread)}</td>
          <td class="diff ${triggerClass}">${
            Number.isFinite(position.currentTriggerSpread)
              ? diffFormat.format(position.currentTriggerSpread)
              : "--"
          }</td>
          <td>${
            Number.isFinite(position.currentExitSpread)
              ? diffFormat.format(position.currentExitSpread)
              : "--"
          }</td>
          <td class="diff ${unrealizedClass}">${
            Number.isFinite(position.currentUnrealizedNet)
              ? diffFormat.format(position.currentUnrealizedNet)
              : "--"
          }</td>
          <td>${formatTimestamp(position.openedAt)}</td>
        </tr>`;
    })
    .join("");

  els.openPositionsWrap.innerHTML = `
    ${decisionSummary}
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Size</th>
          <th>Held</th>
          <th>Entry spread</th>
          <th>Trigger spread</th>
          <th>Exit spread</th>
          <th>Unrealized net</th>
          <th>Opened</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function renderClosedTrades(payload) {
  const trades = payload.closedTrades || [];
  const realizedTotal = Number(payload.realizedCumulativeNet || 0);
  const resetAge = formatDuration(payload.realizedTradesResetAt);
  const survivedRatio = trades.length
    ? trades.filter((trade) => trade.realizedNet > 0).length / trades.length
    : 0;

  const summary = `
    <div class="netTotal paperSummary">
      <div>
        <span>Realized P/L</span>
        <strong>${
          trades.length === 0
            ? "--"
            : diffFormat.format(realizedTotal)
        }</strong>
        <span>${trades.length} closed trades</span>
        <span>${
          trades.length === 0
            ? ""
            : `${plainNumber.format(survivedRatio * 100)}% net positive`
        }</span>
        <span>Elapsed ${resetAge}</span>
      </div>
      <div class="netControls">
        <button id="resetRealizedTrades" type="button">Reset</button>
      </div>
    </div>`;

  if (trades.length === 0) {
    els.closedTradesWrap.innerHTML = `
      ${summary}
      <table class="opportunitiesTable">
        <thead>
          <tr>
            <th>Buy exchange</th>
            <th>Sell exchange</th>
            <th>Size</th>
            <th>Held</th>
            <th>Entry net (est.)</th>
            <th>Realized net</th>
            <th>P/L vs estimated</th>
            <th>Reason</th>
            <th>Closed</th>
          </tr>
        </thead>
        <tbody>
          <tr>
            <td class="diff neutral" colspan="9">No closed trades yet. Positions close when the entry-direction spread crosses zero or after timeout.</td>
          </tr>
        </tbody>
      </table>`;
    return;
  }

  const rows = trades
    .map((trade) => {
      const realizedClass = trade.realizedNet > 0 ? "positive" : "negative";
      const deltaClass = trade.pnlVsEstimated > 0 ? "positive" : "negative";
      return `
        <tr>
          <td>${trade.buyExchange}</td>
          <td>${trade.sellExchange}</td>
          <td>${plainNumber.format(trade.sizeBtc)} BTC</td>
          <td>${trade.ticksHeld} ticks</td>
          <td>${diffFormat.format(trade.entryEstimatedNet)}</td>
          <td class="diff ${realizedClass}">${diffFormat.format(trade.realizedNet)}</td>
          <td class="diff ${deltaClass}">${diffFormat.format(trade.pnlVsEstimated)}</td>
          <td>${trade.closedReason}</td>
          <td>${formatTimestamp(trade.closedAt)}</td>
        </tr>`;
    })
    .join("");

  els.closedTradesWrap.innerHTML = `
    ${summary}
    <table class="opportunitiesTable">
      <thead>
        <tr>
          <th>Buy exchange</th>
          <th>Sell exchange</th>
          <th>Size</th>
          <th>Held</th>
          <th>Entry net (est.)</th>
          <th>Realized net</th>
          <th>P/L vs estimated</th>
          <th>Reason</th>
          <th>Closed</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

async function resetRealizedTrades() {
  await fetch("/api/reset-realized-trades", { method: "POST" });
  await loadTickers();
}

async function loadTickers() {
  if (state.paused) return;

  try {
    const response = await fetch("/api/tickers", { cache: "no-store" });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    const payload = await response.json();
    state.pollMs = payload.pollMs;
    state.lastPayloadAt = Date.now();

    renderSummary(payload);
    renderTable(payload);
    renderOpportunities(payload);
    renderDepthSimulations(payload);
    renderRecordedOpportunities(payload);
    renderSpreadLog(payload);
    renderPaperChecks(payload);
    renderFakeTrades(payload);
    renderOpenPositions(payload);
    renderClosedTrades(payload);

    const liveCount = validPrices(payload.prices).length;
    const age = formatAge(payload.updatedAt);
    setConnection(liveCount > 0 ? "live" : "offline", `${liveCount}/${payload.prices.length} live, ${age}`);
  } catch (error) {
    setConnection("offline", error.message);
  } finally {
    clearTimeout(state.timer);
    state.timer = setTimeout(loadTickers, Math.max(500, Math.min(state.pollMs, 1500)));
  }
}

els.pauseButton.addEventListener("click", () => {
  state.paused = !state.paused;
  els.pauseButton.textContent = state.paused ? "Resume" : "Pause";

  if (state.paused) {
    clearTimeout(state.timer);
    setConnection("", "Paused");
  } else {
    loadTickers();
  }
});

els.colorModeToggle.addEventListener("click", () => {
  state.colorblindMode = els.colorModeToggle.checked;
  localStorage.setItem("colorblindMode", state.colorblindMode ? "1" : "0");
  applyColorMode();
});

document.addEventListener("input", (event) => {
  if (event.target?.id === "minDepthThreshold") {
    state.thresholdDraft = event.target.value;
  }
  if (event.target?.id === "accountCapitalInput") {
    state.capitalDraft = event.target.value;
  }
});

document.addEventListener("click", async (event) => {
  if (event.target?.id === "saveDepthSettings") {
    await saveDepthSettings();
  }
  if (event.target?.id === "resetDepthTotal") {
    await resetDepthTotal();
  }
  if (event.target?.id === "saveCapitalSettings") {
    await saveCapitalSettings();
  }
  if (event.target?.id === "resetFakeTrades") {
    await resetFakeTrades();
  }
  if (event.target?.id === "resetRealizedTrades") {
    await resetRealizedTrades();
  }
});

applyColorMode();
initSectionToggles();
loadTickers();
