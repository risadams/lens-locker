// LensLocker frontend — Milestone 5.
//
// Ports workplan/design/lenslocker-design.html's DOM structure/interaction
// patterns (owner-approved) onto real backend commands. The design's fake
// in-memory 140-item array and naive full-array render are replaced with:
// real SQL-backed paging (`list_images` offset/limit) and a virtualized
// grid — only the visible window (+ a small buffer) of cells is ever built
// in the DOM, per workplan/research/thumbnail-grid-benchmark.md's validated
// approach. Thumbnails are served from disk via Tauri's built-in asset
// protocol (`convertFileSrc`) — see the deviation note below.

const { invoke, convertFileSrc } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

// ── Thumbnail serving — a documented judgment call ──────────────────────
// workplan/research/thumbnail-grid-benchmark.md found that a *hand-rolled*
// custom URI-scheme protocol handler (`register_uri_scheme_protocol`) fails
// silently on Windows/WebView2, and recommends serving thumbnails as plain
// static files instead. This build uses Tauri v2's own built-in, maintained
// `asset:`/`asset.localhost` protocol (via `convertFileSrc`, enabled in
// tauri.conf.json's `assetProtocol` config) rather than a bespoke scheme
// handler or a hand-rolled local HTTP server (which would need `mio`/
// `socket2` — both banned by deny.toml's offline-enforcement policy). This
// is a different code path from the one the benchmark found broken, but it
// is still a custom-scheme-shaped mechanism, so it was verified empirically
// during this milestone's driven end-to-end run rather than assumed safe —
// see the build report for the observed result.
function assetSrc(path) {
  return path ? convertFileSrc(path) : '';
}

function vg(size, cls) {
  return `<svg class="${cls}" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"><path d="M12 2l7 3v6c0 5-3.5 8.5-7 10-3.5-1.5-7-5-7-10V5z"/><path d="M9 12l2 2 4-4"/></svg>`;
}
function checkIcon() { return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><path d="M5 12l5 5 9-9"/></svg>`; }
function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}
function fmtDate(iso) { return iso ? iso.slice(0, 10) : '—'; }
function fmtSize(bytes) { return bytes == null ? '—' : (bytes / 1_000_000).toFixed(1) + ' MB'; }
function fmtGb(bytes) { return (bytes / 1_000_000_000).toFixed(1); }

// ── Filter / sort / search state ─────────────────────────────────────────
const state = {
  dateRange: null,           // {label, from: 'YYYY-MM-DD'|null, to: 'YYYY-MM-DD'|null}
  formats: new Set(),
  sources: new Set(),
  tags: new Set(),
  sort: 'captured-desc',
  query: '',
};

const DATE_PRESETS = [
  { label: 'Today', days: 1 },
  { label: 'This week', days: 7 },
  { label: 'This month', days: 31 },
  { label: 'This year', days: 366 },
];
const SORT_OPTIONS = [
  { key: 'captured-desc', label: 'Newest captured' },
  { key: 'captured-asc', label: 'Oldest captured' },
  { key: 'imported-desc', label: 'Recently imported' },
  { key: 'filename-asc', label: 'Filename (A–Z)' },
  { key: 'size-desc', label: 'File size (largest)' },
];

function isoDaysAgo(n) {
  const d = new Date(Date.now() - n * 86400000);
  return d.toISOString().slice(0, 10);
}

function filtersDto() {
  return {
    dateFrom: state.dateRange?.from ?? null,
    dateTo: state.dateRange?.to ?? null,
    formats: [...state.formats],
    sources: [...state.sources],
    tags: [...state.tags],
  };
}

// ── Grid: real paging + virtualization ───────────────────────────────────
const PAGE = 90;
const BUFFER_ROWS = 3;
const CELL_MIN = 150;

// `total` is `null` until the first `list_images` response for the current
// filter/sort/search combination lands — distinct from `0`, which means "a
// real response confirmed there are no matches." Collapsing these two
// states (an early version of this file did) meant the very first render
// pass short-circuited into the "no results" empty state before ever
// issuing the first fetch — total stayed 0 forever, since the fetch that
// would have set it real was never triggered. Found via this milestone's
// driven CDP run against the real app, not just reasoned about.
let total = null;
let itemCache = new Map();     // index -> GridImageDto
let pendingPages = new Set();  // page-start offsets currently in flight
let requestToken = 0;          // bumped on every filter/sort/search change to invalidate stale responses
let columns = 1;

// ── Grid multi-select (ML-SPEC.md §5) ────────────────────────────────────
// A generic primitive, not narrowly scoped to tags — §5: "reuses one
// shared multi-select primitive that §6's face-cluster splitting also
// needs... build it generically enough to serve both call sites." §6's
// face-crop-thumbnail reuse is a later slice; this is the grid half.
//
// Interaction design (checkbox-on-hover + shift-range, a bulk bar once
// 1+ selected) is this build's own judgment call — ML-SPEC.md deliberately
// specifies *that* a multi-select primitive is needed, not its exact
// mechanics. Follows the common photo-app convention (Google/Apple
// Photos): once 1+ items are selected, clicking a thumb's body continues
// selecting instead of opening the drawer, so a half-selected state can't
// accidentally be abandoned by a stray click.
const bulkSelection = new Set(); // image ids, not indices (ids survive re-sorts/scrolls; indices don't)
let lastClickedIdx = null;       // for shift-click range selection

const gridWrap = document.getElementById('gridWrap');
const gridSpacer = document.getElementById('gridSpacer');
const gridWindow = document.getElementById('gridWindow');
const countEl = document.getElementById('itemCount');

function computeColumns() {
  columns = Math.max(1, Math.floor(gridWrap.clientWidth / CELL_MIN));
}
function cellSize() {
  return columns > 0 ? gridWrap.clientWidth / columns : 0;
}
function totalRows() {
  return Math.ceil((total ?? 0) / columns);
}

function resetGridData() {
  itemCache = new Map();
  pendingPages = new Set();
  total = null;
  requestToken++;
  gridWrap.scrollTop = 0;
}

async function fetchPage(offset, token) {
  if (pendingPages.has(offset)) return;
  pendingPages.add(offset);
  try {
    const res = await invoke('list_images', {
      filters: filtersDto(),
      sort: state.sort,
      search: state.query || null,
      offset,
      limit: PAGE,
    });
    if (token !== requestToken) return; // stale — a newer filter/sort/search superseded this request
    total = res.total;
    res.items.forEach((item, i) => itemCache.set(offset + i, item));
    layout();
  } catch (e) {
    console.error('list_images failed', e);
  } finally {
    pendingPages.delete(offset);
  }
}

function ensureRange(first, last, token) {
  const startPage = Math.floor(first / PAGE) * PAGE;
  for (let p = startPage; p < last; p += PAGE) {
    if (!itemCache.has(p)) fetchPage(p, token);
  }
}

function renderCellHtml(idx) {
  const item = itemCache.get(idx);
  if (!item) return `<div class="thumb" data-idx="${idx}"></div>`;
  const tagsHtml = item.tags.length
    ? `<div class="thumb-tags">${item.tags.map(t => `<span class="thumb-tag">${escapeHtml(t)}</span>`).join('')}</div>`
    : '';
  const src = assetSrc(item.thumbnailPath);
  const img = src ? `<img src="${src}" loading="lazy" alt="">` : `<div class="fake-img"></div>`;
  const picked = bulkSelection.has(item.id);
  return `<div class="thumb${picked ? ' thumb-picked' : ''}" data-id="${item.id}" data-idx="${idx}">
    ${img}
    ${item.verified ? vg(13, 'verified-glyph always') : ''}
    <div class="thumb-select-dot"></div>
    <div class="thumb-pick" data-pick title="Select">${picked ? checkIcon() : ''}</div>
    <div class="thumb-overlay">
      <div class="thumb-meta"><span class="thumb-date">${fmtDate(item.captureDate)}</span>${item.verified ? vg(12, 'verified-glyph') : ''}</div>
      ${tagsHtml}
    </div>
  </div>`;
}

let renderScheduled = false;
function scheduleRenderWindow() {
  if (renderScheduled) return;
  renderScheduled = true;
  requestAnimationFrame(() => {
    renderScheduled = false;
    renderWindow();
  });
}

function layout() {
  computeColumns();
  const size = cellSize();
  gridSpacer.style.height = size ? `${totalRows() * size}px` : '0px';
  renderWindow();
  updateCount();
}

function renderWindow() {
  const token = requestToken;
  const size = cellSize();
  if (currentView !== 'grid') return;

  // total === null means "no response for this query has landed yet" —
  // distinct from a confirmed-empty result (total === 0). Must still fetch
  // page 0 in that case, not short-circuit into the empty-state message
  // (see the `total` declaration's comment for the bug this fixes).
  if (total === null) {
    ensureRange(0, PAGE, token);
    gridWindow.style.transform = 'translateY(0px)';
    return;
  }

  if (total === 0) {
    gridWindow.style.transform = 'translateY(0px)';
    gridWindow.innerHTML = hasActiveQuery()
      ? `<div class="empty-results">Nothing matches these filters.<br><button class="popover-link" id="emptyClearBtn" style="margin-top:6px">Clear filters</button></div>`
      : `<div class="empty-results">No photos yet.<br><button class="popover-link" id="emptyImportBtn" style="margin-top:6px">Import a folder</button></div>`;
    document.getElementById('emptyClearBtn')?.addEventListener('click', clearAllFilters);
    document.getElementById('emptyImportBtn')?.addEventListener('click', openImportModal);
    return;
  }
  if (!size) return;

  const scrollTop = gridWrap.scrollTop;
  const viewportRows = Math.ceil(gridWrap.clientHeight / size) + BUFFER_ROWS * 2;
  const firstRow = Math.max(0, Math.floor(scrollTop / size) - BUFFER_ROWS);
  const firstIndex = firstRow * columns;
  const lastIndex = Math.min(total, firstIndex + viewportRows * columns);

  ensureRange(firstIndex, lastIndex, token);

  gridWindow.style.transform = `translateY(${firstRow * size}px)`;
  const cells = [];
  for (let idx = firstIndex; idx < lastIndex; idx++) cells.push(renderCellHtml(idx));
  gridWindow.innerHTML = cells.join('');
}

gridWindow.addEventListener('click', (e) => {
  const cell = e.target.closest('.thumb[data-id]');
  if (!cell) return;
  const id = Number(cell.dataset.id);
  const idx = Number(cell.dataset.idx);
  const onPickTarget = !!e.target.closest('[data-pick]');

  // The checkbox always selects; the thumb's body only selects once 1+
  // items are already selected — "once selection is active, clicking a
  // thumb's body continues selecting instead of opening the drawer"
  // (module doc comment above bulkSelection's declaration) — a stray
  // click can't silently abandon a half-built selection. Otherwise, a
  // body click opens the drawer as it always has.
  if (onPickTarget || bulkSelection.size > 0) {
    if (onPickTarget) e.stopPropagation();
    if (e.shiftKey && lastClickedIdx !== null) {
      selectRange(lastClickedIdx, idx);
    } else {
      toggleBulkSelection(id);
    }
    lastClickedIdx = idx;
    return;
  }

  openDrawer(id);
});

function toggleBulkSelection(id) {
  bulkSelection.has(id) ? bulkSelection.delete(id) : bulkSelection.add(id);
  updateBulkBar();
  scheduleRenderWindow();
}

// Selects every *currently loaded* item between fromIdx and toIdx
// (inclusive) — a disclosed limitation, not a bug: indices outside what's
// been scrolled-to/fetched yet (this grid only loads its visible window +
// buffer, per the file's own top-of-file virtualization note) can't be
// included in a range that was never fetched.
function selectRange(fromIdx, toIdx) {
  const [lo, hi] = fromIdx <= toIdx ? [fromIdx, toIdx] : [toIdx, fromIdx];
  for (let i = lo; i <= hi; i++) {
    const item = itemCache.get(i);
    if (item) bulkSelection.add(item.id);
  }
  updateBulkBar();
  scheduleRenderWindow();
}

function clearBulkSelection() {
  bulkSelection.clear();
  lastClickedIdx = null;
  updateBulkBar();
  scheduleRenderWindow();
}

function updateBulkBar() {
  const bar = document.getElementById('bulkBar');
  if (!bar) return;
  bar.style.display = bulkSelection.size > 0 ? 'flex' : 'none';
  const countEl = document.getElementById('bulkCount');
  if (countEl) countEl.textContent = `${bulkSelection.size} selected`;
}

gridWrap.addEventListener('scroll', scheduleRenderWindow);
window.addEventListener('resize', () => layout());

function hasActiveQuery() {
  return !!(state.dateRange || state.formats.size || state.sources.size || state.tags.size || state.query);
}

function updateCount() {
  countEl.textContent = total === null ? '…' : `${total} item${total === 1 ? '' : 's'}`;
}

function refreshGrid() {
  resetGridData();
  layout();
}

// ── Filter bar / popovers ────────────────────────────────────────────────
function closeAllPops(except) {
  document.querySelectorAll('.popover.open').forEach(p => { if (p.id !== except) p.classList.remove('open'); });
}
function togglePop(id, evt) {
  evt.stopPropagation();
  const el = document.getElementById(id);
  const wasOpen = el.classList.contains('open');
  closeAllPops(id);
  el.classList.toggle('open', !wasOpen);
}
document.addEventListener('click', () => closeAllPops());

async function renderFilterBar() {
  const bar = document.getElementById('filterBar');
  const dateOn = !!state.dateRange, fmtOn = state.formats.size > 0, srcOn = state.sources.size > 0, tagOn = state.tags.size > 0;
  bar.innerHTML = `
    <div class="chip ${dateOn ? 'on' : ''}" id="dateChip">
      ${state.dateRange ? escapeHtml(state.dateRange.label) : 'Date'}
      <div class="popover" id="datePop"></div>
    </div>
    <div class="chip ${fmtOn ? 'on' : ''}" id="fmtChip">
      Format${fmtOn ? ` (${state.formats.size})` : ''}
      <div class="popover" id="fmtPop"></div>
    </div>
    <div class="chip ${srcOn ? 'on' : ''}" id="srcChip">
      Source${srcOn ? ` (${state.sources.size})` : ''}
      <div class="popover" id="srcPop"></div>
    </div>
    <div class="chip ${tagOn ? 'on' : ''}" id="tagChip">
      Tags${tagOn ? ` (${state.tags.size})` : ''}
      <div class="popover" id="tagPop"></div>
    </div>
  `;
  document.getElementById('dateChip').addEventListener('click', (e) => togglePop('datePop', e));
  document.getElementById('fmtChip').addEventListener('click', (e) => togglePop('fmtPop', e));
  document.getElementById('srcChip').addEventListener('click', (e) => togglePop('srcPop', e));
  document.getElementById('tagChip').addEventListener('click', (e) => togglePop('tagPop', e));
  [...bar.querySelectorAll('.popover')].forEach(p => p.addEventListener('click', (e) => e.stopPropagation()));

  renderDatePop();
  await Promise.all([renderFormatPop(), renderSourcePop(), renderTagPop()]);
}

function renderDatePop() {
  const pop = document.getElementById('datePop');
  pop.innerHTML = DATE_PRESETS.map(p => {
    const on = state.dateRange && state.dateRange.label === p.label;
    return `<div class="popover-item ${on ? 'checked' : ''}" data-preset="${p.label}"><span class="box">${on ? checkIcon() : ''}</span><span class="label">${p.label}</span></div>`;
  }).join('') + `
    <div class="popover-divider"></div>
    <div class="popover-date-custom"><input type="date" id="dateFrom"><span style="color:var(--text-faint)">–</span><input type="date" id="dateTo"></div>
    <div class="popover-foot"><button class="popover-link" id="clearDateBtn">Clear</button><button class="popover-link" id="applyDateBtn">Apply</button></div>
  `;
  pop.querySelectorAll('[data-preset]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    const preset = DATE_PRESETS.find(p => p.label === el.dataset.preset);
    state.dateRange = { label: preset.label, from: isoDaysAgo(preset.days), to: null };
    refresh();
  }));
  pop.querySelector('#clearDateBtn').addEventListener('click', (e) => { e.stopPropagation(); state.dateRange = null; refresh(); });
  pop.querySelector('#applyDateBtn').addEventListener('click', (e) => {
    e.stopPropagation();
    const from = pop.querySelector('#dateFrom').value, to = pop.querySelector('#dateTo').value;
    if (!from && !to) return;
    state.dateRange = { label: `${from || '…'} – ${to || '…'}`, from: from || null, to: to || null };
    refresh();
  });
}

async function renderFormatPop() {
  const pop = document.getElementById('fmtPop');
  // No dedicated "distinct formats" command was in this milestone's scope;
  // the tag/source popovers have server-computed counts (list_tags/
  // list_sources) — formats reuse the fixed §5 format-matrix list instead,
  // toggled directly without server-side counts.
  const formats = ['jpeg', 'png', 'webp', 'gif', 'bmp', 'tiff', 'jxl', 'cr2', 'nef', 'arw', 'dng', 'rw2', 'raf', 'orf'];
  pop.innerHTML = formats.map(f => {
    const on = state.formats.has(f);
    return `<div class="popover-item ${on ? 'checked' : ''}" data-fmt="${f}"><span class="box">${on ? checkIcon() : ''}</span><span class="label">${f.toUpperCase()}</span></div>`;
  }).join('') + `<div class="popover-divider"></div><div class="popover-foot"><button class="popover-link" id="clearFmtBtn">None</button><span></span></div>`;
  pop.querySelectorAll('[data-fmt]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    const f = el.dataset.fmt;
    state.formats.has(f) ? state.formats.delete(f) : state.formats.add(f);
    refresh();
  }));
  pop.querySelector('#clearFmtBtn').addEventListener('click', (e) => { e.stopPropagation(); state.formats.clear(); refresh(); });
}

async function renderSourcePop() {
  const pop = document.getElementById('srcPop');
  let sources = [];
  try { sources = await invoke('list_sources'); } catch (e) { console.error(e); }
  pop.innerHTML = `<div style="font-size:10px;color:var(--text-faint);padding:4px 8px 6px">Where a photo was originally imported from</div>` +
    sources.map(s => {
      const on = state.sources.has(s.sourceRoot);
      return `<div class="popover-item ${on ? 'checked' : ''}" data-src="${escapeHtml(s.sourceRoot)}" title="${escapeHtml(s.sourceRoot)}">
        <span class="box">${on ? checkIcon() : ''}</span><span class="label" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeHtml(s.sourceRoot)}</span><span class="n">${s.count}</span>
      </div>`;
    }).join('');
  pop.querySelectorAll('[data-src]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    const s = el.dataset.src;
    state.sources.has(s) ? state.sources.delete(s) : state.sources.add(s);
    refresh();
  }));
}

async function renderTagPop() {
  const pop = document.getElementById('tagPop');
  let tags = [];
  try { tags = await invoke('list_tags'); } catch (e) { console.error(e); }
  pop.innerHTML = tags.map(t => {
    const on = state.tags.has(t.name);
    return `<div class="popover-item ${on ? 'checked' : ''}" data-tag="${escapeHtml(t.name)}"><span class="box">${on ? checkIcon() : ''}</span><span class="label">${escapeHtml(t.name)}</span><span class="n">${t.count}</span></div>`;
  }).join('');
  pop.querySelectorAll('[data-tag]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    const t = el.dataset.tag;
    state.tags.has(t) ? state.tags.delete(t) : state.tags.add(t);
    refresh();
  }));
}

function renderActiveFilters() {
  const pills = [];
  if (state.dateRange) pills.push({ text: state.dateRange.label, clear: () => { state.dateRange = null; } });
  state.formats.forEach(f => pills.push({ text: f.toUpperCase(), clear: () => state.formats.delete(f) }));
  state.sources.forEach(s => pills.push({ text: s.split(/[\\/]/).pop(), clear: () => state.sources.delete(s) }));
  state.tags.forEach(t => pills.push({ text: '#' + t, clear: () => state.tags.delete(t) }));

  const bar = document.getElementById('activeFilters');
  if (!pills.length) { bar.classList.remove('show'); bar.innerHTML = ''; return; }
  bar.classList.add('show');
  bar.innerHTML = `<span class="active-filters-label">Filtered by</span>` +
    pills.map((p, idx) => `<span class="pill">${escapeHtml(p.text)}<button data-idx="${idx}"><svg viewBox="0 0 24 24" width="9" height="9" fill="none" stroke="currentColor" stroke-width="3"><path d="M6 6l12 12M18 6L6 18"/></svg></button></span>`).join('') +
    `<button class="clear-all" id="clearAllBtn">Clear all</button>`;
  bar.querySelectorAll('button[data-idx]').forEach((btn, idx) => btn.addEventListener('click', () => { pills[idx].clear(); refresh(); }));
  bar.querySelector('#clearAllBtn').addEventListener('click', clearAllFilters);
}
function clearAllFilters() {
  state.dateRange = null; state.formats.clear(); state.sources.clear(); state.tags.clear();
  refresh();
}

function renderSortPop() {
  const pop = document.getElementById('sortPop');
  pop.innerHTML = SORT_OPTIONS.map(o => {
    const on = state.sort === o.key;
    return `<div class="popover-item ${on ? 'checked' : ''}" data-sort="${o.key}"><span class="box">${on ? checkIcon() : ''}</span><span class="label">${o.label}</span></div>`;
  }).join('');
  pop.querySelectorAll('[data-sort]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    state.sort = el.dataset.sort;
    document.getElementById('sortLabel').textContent = SORT_OPTIONS.find(o => o.key === state.sort).label;
    closeAllPops();
    renderSortPop();
    refreshGrid();
  }));
}
document.getElementById('sortChip').addEventListener('click', (e) => togglePop('sortPop', e));

let searchDebounce = null;
document.getElementById('searchInput').addEventListener('input', (e) => {
  clearTimeout(searchDebounce);
  const value = e.target.value.trim();
  searchDebounce = setTimeout(() => { state.query = value; refreshGrid(); }, 180);
});

async function refresh() {
  await renderFilterBar();
  renderActiveFilters();
  refreshGrid();
}

// ── Detail drawer ─────────────────────────────────────────────────────────
let currentDetail = null;

async function openDrawer(id) {
  document.querySelectorAll('.thumb.selected').forEach(t => t.classList.remove('selected'));
  document.querySelector(`.thumb[data-id="${id}"]`)?.classList.add('selected');

  let detail;
  try { detail = await invoke('get_image_detail', { id }); } catch (e) { showToast('Could not load image details'); return; }
  currentDetail = detail;

  const preview = document.getElementById('drawerPreview');
  preview.querySelector('img')?.remove();
  const img = document.createElement('img');
  img.src = assetSrc(thumbnailPathFor(id));
  preview.prepend(img);

  document.getElementById('d-filename').textContent = detail.filename;
  document.getElementById('d-sub').textContent = detail.storedFormat !== detail.originalFormat
    ? `Converted from ${detail.originalFormat.toUpperCase()}${detail.width ? ` · ${detail.width}×${detail.height}` : ''}`
    : `${detail.originalFormat.toUpperCase()}${detail.width ? ` · ${detail.width}×${detail.height}` : ''}`;
  document.getElementById('d-camera').textContent = [detail.cameraMake, detail.cameraModel].filter(Boolean).join(' ') || '—';
  document.getElementById('d-hash').textContent = detail.originalHashHex.slice(0, 10) + '…' + detail.originalHashHex.slice(-4);
  document.getElementById('d-captured').textContent = detail.captureDate ? detail.captureDate.replace('T', ' ').slice(0, 19) : '—';
  document.getElementById('d-size').textContent = fmtSize(detail.fileSizeBytes);
  document.getElementById('d-imported').textContent = detail.firstImportedAt ? detail.firstImportedAt.slice(0, 10) : '—';

  renderDrawerTags(detail);
  renderDrawerPeople(detail);

  document.getElementById('drawer').classList.add('open');
  document.getElementById('drawerScrim').classList.add('open');
}

// "People in this photo" (028 decision #5): named faces are individual
// clickable chips (reusing the tag-chip look — no bounding-box overlay,
// per 028's own reasoning against one), each jumping to that person's
// cluster card in the People view. Unnamed/unclustered detections
// collapse into one "+N unidentified" chip — clicking it names inline
// (028 decision #3) when there's exactly one unnamed cluster on this
// image and nothing left unclustered (the only case a target is
// unambiguous without a per-face-crop picker, which 028 decision #5 rules
// out via its no-bounding-box-overlay call); otherwise it falls back to
// the People view, since which of several unnamed clusters the user means
// genuinely can't be told apart here.
function renderDrawerPeople(detail) {
  const el = document.getElementById('d-people');
  const totalUnnamed = detail.unnamedClustered.reduce((sum, g) => sum + g.count, 0) + detail.unclusteredFaceCount;
  const soleUnnamedCluster = detail.unclusteredFaceCount === 0 && detail.unnamedClustered.length === 1
    ? detail.unnamedClustered[0].clusterId
    : null;

  const chips = detail.namedFaces.map(f =>
    `<span class="tag-chip" data-jump-cluster="${f.clusterId}" style="cursor:pointer">${escapeHtml(f.personName)}</span>`
  );
  if (totalUnnamed > 0) {
    if (soleUnnamedCluster !== null) {
      chips.push(`<span class="tag-chip" data-name-unnamed="${soleUnnamedCluster}" style="cursor:pointer">+${totalUnnamed} unidentified</span>`);
    } else {
      chips.push(`<span class="tag-chip" data-jump-people style="cursor:pointer">+${totalUnnamed} unidentified</span>`);
    }
  }
  el.innerHTML = chips.join('') || `<span style="color:var(--text-faint);font-size:11.5px">No faces detected</span>`;

  el.querySelectorAll('[data-jump-cluster]').forEach(chip => chip.addEventListener('click', (e) => {
    e.stopPropagation();
    const clusterId = Number(chip.dataset.jumpCluster);
    closeDrawer();
    jumpToCluster(clusterId);
  }));
  el.querySelectorAll('[data-jump-people]').forEach(chip => chip.addEventListener('click', (e) => {
    e.stopPropagation();
    closeDrawer();
    switchView('people');
  }));
  const nameUnnamedChip = el.querySelector('[data-name-unnamed]');
  if (nameUnnamedChip) {
    nameUnnamedChip.addEventListener('click', (e) => {
      e.stopPropagation();
      promptTagNameInput(nameUnnamedChip, async (value) => {
        try {
          await invoke('name_face_cluster', { clusterId: Number(nameUnnamedChip.dataset.nameUnnamed), personName: value });
          showToast(`Named — ${value}`);
          openDrawer(detail.id);
        } catch (err) { showToast('Could not name this person'); }
      }, { placeholder: 'person’s name…', listId: 'personNames' });
    });
  }
}

function thumbnailPathFor(id) {
  // The grid cache already has thumbnail paths for whatever's currently
  // loaded; it's keyed by index, not id, so scan it rather than round-trip
  // to the backend again for a value the frontend already has in memory.
  for (const item of itemCache.values()) if (item.id === id) return item.thumbnailPath;
  return null;
}

// detail.tags is [{name, source, confidence, reviewState}] — source is
// 'manual'|'auto', reviewState is 'unreviewed'|'confirmed'|null (null for
// manual tags). See ImageDetailDto/TagDto (src-tauri/src/lib.rs).
function renderDrawerTags(detail) {
  const el = document.getElementById('d-tags');
  el.innerHTML = detail.tags.map(t => {
    const unreviewed = t.source === 'auto' && t.reviewState === 'unreviewed';
    const confirmBtn = unreviewed ? `<button data-confirm="${escapeHtml(t.name)}" title="Confirm">✓</button>` : '';
    return `<span class="tag-chip${unreviewed ? ' tag-chip-unreviewed' : ''}">${escapeHtml(t.name)}${confirmBtn}<button data-tag="${escapeHtml(t.name)}">×</button></span>`;
  }).join('')
    + `<button class="tag-add" id="tagAddBtn">+ Add tag</button>`;

  el.querySelectorAll('button[data-confirm]').forEach(btn => btn.addEventListener('click', async (e) => {
    e.stopPropagation();
    await invoke('confirm_auto_tag', { imageId: detail.id, tag: btn.dataset.confirm });
    const t = detail.tags.find(t => t.name === btn.dataset.confirm);
    if (t) t.reviewState = 'confirmed';
    // No invalidateItemTags/renderTagPop here, unlike the add/reject
    // handlers below: confirming changes neither a tag's name nor which
    // images carry it, so the grid cache and tag-filter popover counts
    // have nothing to invalidate.
    renderDrawerTags(detail);
  }));
  el.querySelectorAll('button[data-tag]').forEach(btn => btn.addEventListener('click', async (e) => {
    e.stopPropagation();
    // Rejecting an auto-tag persists the rejection (won't be silently
    // re-suggested on a later re-score); removing a manual tag has no
    // such memory to keep (ML-SPEC.md §5). Looked up from detail.tags
    // (like the confirm handler above) rather than a parallel data-auto
    // attribute — one source of truth for each tag's provenance.
    const isAuto = detail.tags.find(t => t.name === btn.dataset.tag)?.source === 'auto';
    const command = isAuto ? 'reject_auto_tag' : 'remove_tag';
    await invoke(command, { imageId: detail.id, tag: btn.dataset.tag });
    detail.tags = detail.tags.filter(t => t.name !== btn.dataset.tag);
    renderDrawerTags(detail);
    invalidateItemTags(detail.id, detail.tags.map(t => t.name));
    renderTagPop();
  }));
  el.querySelector('#tagAddBtn').addEventListener('click', (e) => {
    e.stopPropagation();
    promptTagNameInput(el.querySelector('#tagAddBtn'), async (value) => {
      if (!detail.tags.some(t => t.name === value)) {
        await invoke('add_tag', { imageId: detail.id, tag: value });
        detail.tags.push({ name: value, source: 'manual', confidence: null, reviewState: null });
        detail.tags.sort((a, b) => a.name.localeCompare(b.name));
      }
      renderDrawerTags(detail);
      invalidateItemTags(detail.id, detail.tags.map(t => t.name));
      renderTagPop();
    });
  });
}

// Shared by the drawer's "+ Add tag" flow above, the bulk-bar's "Add
// tag"/"Remove tag" flows, and the People view's naming flow below — all
// need the exact same create-input/focus/commit-on-Enter-or-blur/Escape-
// discards lifecycle; factored out once rather than parallel-implemented
// per call site. Replaces `triggerEl` with a text input; on Enter or blur
// with a non-empty trimmed value, restores `triggerEl` and calls
// `onCommit(value)`; on Escape, or an empty Enter/blur, just restores
// `triggerEl` unchanged (Escape always discards, even if something was
// typed — a different rule from blur/Enter, which only "cancel" when
// there was nothing to commit). `listId`, when given, wires the input to
// a `<datalist>` for native-browser autocomplete (028 decision #3's
// "autocompletes against already-named people") — no custom dropdown
// widget, matching the frontend's no-new-primitives-where-avoidable posture.
function promptTagNameInput(triggerEl, onCommit, { placeholder = 'tag name…', listId = null } = {}) {
  const input = document.createElement('input');
  input.className = 'tag-input';
  input.placeholder = placeholder;
  if (listId) input.setAttribute('list', listId);
  triggerEl.replaceWith(input);
  input.focus();
  let settled = false;
  const restore = () => { if (input.isConnected) input.replaceWith(triggerEl); };
  const commit = async () => {
    if (settled) return;
    settled = true;
    const value = input.value.trim();
    restore();
    if (value) await onCommit(value);
  };
  const cancel = () => {
    if (settled) return;
    settled = true;
    restore();
  };
  input.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter') commit();
    if (ev.key === 'Escape') cancel();
  });
  input.addEventListener('blur', commit);
}

function invalidateItemTags(id, tags) {
  for (const item of itemCache.values()) if (item.id === id) item.tags = tags;
  scheduleRenderWindow();
}

function closeDrawer() {
  document.getElementById('drawer').classList.remove('open');
  document.getElementById('drawerScrim').classList.remove('open');
  document.querySelectorAll('.thumb.selected').forEach(t => t.classList.remove('selected'));
}
document.getElementById('drawerCloseBtn').addEventListener('click', (e) => { e.stopPropagation(); closeDrawer(); });
document.getElementById('drawerScrim').addEventListener('click', closeDrawer);
document.getElementById('drawerExpandBtn').addEventListener('click', (e) => { e.stopPropagation(); if (currentDetail) openLightbox(currentDetail.id); });
document.getElementById('drawerPreview').addEventListener('click', () => { if (currentDetail) openLightbox(currentDetail.id); });

// ── Lightbox: full-size view, prev/next, scroll-to-zoom + pan ───────────
// Ported near-verbatim from the approved design's interaction logic; only
// the image-source resolution is real (backend-driven) instead of a CSS
// gradient placeholder.
const lightboxEl = document.getElementById('lightbox');
const lbImgEl = document.getElementById('lb-img');
const lbStageEl = document.getElementById('lb-stage');
let lbZoom = { scale: 1, tx: 0, ty: 0 };
let lbPanning = false, lbPanStart = { x: 0, y: 0, tx: 0, ty: 0 };
let lbCurrentId = null;

async function openLightbox(id) {
  lbCurrentId = id;
  lbZoom = { scale: 1, tx: 0, ty: 0 };
  lbImgEl.style.transformOrigin = '50% 50%';
  await renderLightbox();
  lightboxEl.classList.add('open');
}
function closeLightbox() { lightboxEl.classList.remove('open'); }

async function renderLightbox() {
  const detail = await invoke('get_image_detail', { id: lbCurrentId });
  const requestedId = lbCurrentId;

  // Show the grid thumbnail immediately (already on disk, instant), then
  // swap in a full-resolution render once the backend finishes decoding
  // it. `get_full_preview` generates it fresh on every call — nothing is
  // cached to disk (see its doc: caching every viewed photo's full-res
  // render used to roughly double the vault's disk footprint) — so this is
  // a genuine async decode, not a cache lookup.
  lbImgEl.src = assetSrc(thumbnailPathFor(lbCurrentId));
  document.getElementById('lb-filename').textContent = detail.filename;

  const idx = currentVisibleIndex(lbCurrentId);
  document.getElementById('lb-counter').textContent = idx >= 0 ? `${idx + 1} of ${total}` : '';
  applyLbTransform();

  try {
    const dataUrl = await invoke('get_full_preview', { id: requestedId });
    // Only apply if still viewing the same image — the user may have
    // paged on to another one while this was decoding.
    if (dataUrl && lbCurrentId === requestedId) {
      lbImgEl.src = dataUrl;
    }
  } catch (e) {
    // Full-resolution render failed (e.g. a RAW file) — the thumbnail
    // already showing is an acceptable fallback.
  }
}

function currentVisibleIndex(id) {
  for (const [idx, item] of itemCache.entries()) if (item.id === id) return idx;
  return -1;
}

async function lightboxStep(delta) {
  const idx = currentVisibleIndex(lbCurrentId);
  if (idx < 0) return;
  const nextIdx = idx + delta;
  if (nextIdx < 0 || nextIdx >= total) return;
  if (!itemCache.has(nextIdx)) { await fetchPage(Math.floor(nextIdx / PAGE) * PAGE, requestToken); }
  const nextItem = itemCache.get(nextIdx);
  if (!nextItem) return;
  lbCurrentId = nextItem.id;
  lbZoom = { scale: 1, tx: 0, ty: 0 };
  lbImgEl.style.transformOrigin = '50% 50%';
  await renderLightbox();
}

function applyLbTransform() {
  lbImgEl.style.transform = `translate(${lbZoom.tx}px, ${lbZoom.ty}px) scale(${lbZoom.scale})`;
  document.getElementById('lb-zoompct').textContent = Math.round(lbZoom.scale * 100) + '%';
  lbStageEl.style.cursor = lbZoom.scale > 1 ? 'grab' : 'default';
}
function lightboxResetZoom() {
  lbZoom = { scale: 1, tx: 0, ty: 0 };
  lbImgEl.style.transformOrigin = '50% 50%';
  applyLbTransform();
}
function lightboxZoomBy(delta, clientX, clientY) {
  const prevScale = lbZoom.scale;
  const newScale = Math.min(4, Math.max(1, prevScale + delta));
  if (newScale === prevScale) return;
  if (clientX !== undefined) {
    const rect = lbImgEl.getBoundingClientRect();
    const originX = Math.min(100, Math.max(0, ((clientX - rect.left) / rect.width) * 100));
    const originY = Math.min(100, Math.max(0, ((clientY - rect.top) / rect.height) * 100));
    lbImgEl.style.transformOrigin = `${originX}% ${originY}%`;
  } else {
    lbImgEl.style.transformOrigin = '50% 50%';
  }
  lbZoom.scale = newScale;
  if (newScale === 1) { lbZoom.tx = 0; lbZoom.ty = 0; lbImgEl.style.transformOrigin = '50% 50%'; }
  applyLbTransform();
}

lbStageEl.addEventListener('wheel', (e) => { e.preventDefault(); lightboxZoomBy(e.deltaY < 0 ? 0.18 : -0.18, e.clientX, e.clientY); }, { passive: false });
lbStageEl.addEventListener('dblclick', () => lightboxResetZoom());
lbStageEl.addEventListener('mousedown', (e) => {
  if (lbZoom.scale <= 1) return;
  lbPanning = true;
  lbStageEl.classList.add('panning');
  lbPanStart = { x: e.clientX, y: e.clientY, tx: lbZoom.tx, ty: lbZoom.ty };
});
window.addEventListener('mousemove', (e) => {
  if (!lbPanning) return;
  lbZoom.tx = lbPanStart.tx + (e.clientX - lbPanStart.x);
  lbZoom.ty = lbPanStart.ty + (e.clientY - lbPanStart.y);
  applyLbTransform();
});
window.addEventListener('mouseup', () => { lbPanning = false; lbStageEl.classList.remove('panning'); });

document.addEventListener('keydown', (e) => {
  if (!lightboxEl.classList.contains('open')) return;
  if (e.key === 'ArrowRight') lightboxStep(1);
  else if (e.key === 'ArrowLeft') lightboxStep(-1);
  else if (e.key === 'Escape') closeLightbox();
  else if (e.key === '+' || e.key === '=') lightboxZoomBy(0.25);
  else if (e.key === '-') lightboxZoomBy(-0.25);
  else if (e.key === '0') lightboxResetZoom();
});

document.getElementById('lbPrevBtn').addEventListener('click', (e) => { e.stopPropagation(); lightboxStep(-1); });
document.getElementById('lbNextBtn').addEventListener('click', (e) => { e.stopPropagation(); lightboxStep(1); });
document.getElementById('lbZoomOutBtn').addEventListener('click', () => lightboxZoomBy(-0.25));
document.getElementById('lbZoomInBtn').addEventListener('click', () => lightboxZoomBy(0.25));
document.getElementById('lb-zoompct').addEventListener('click', lightboxResetZoom);
document.getElementById('lbCloseBtn').addEventListener('click', closeLightbox);

// ── Copy path / export — real file-system actions ────────────────────────
let toastTimer = null;
function showToast(msg) {
  const t = document.getElementById('toast');
  t.innerHTML = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3"><path d="M5 12l5 5 9-9"/></svg>${escapeHtml(msg)}`;
  t.classList.add('show');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove('show'), 1800);
}

document.getElementById('lbCopyBtn').addEventListener('click', async () => {
  if (!lbCurrentId) return;
  try {
    const path = await invoke('copy_file_path', { id: lbCurrentId });
    // WebView2 supports the standard Clipboard API directly — tried first,
    // no extra plugin needed; this held up in this milestone's driven run.
    await navigator.clipboard.writeText(path);
    showToast('File path copied');
  } catch (e) {
    showToast('Could not copy file path');
  }
});

document.getElementById('lbExportBtn').addEventListener('click', async () => {
  if (!lbCurrentId) return;
  try {
    const dest = await invoke('export_image', { id: lbCurrentId });
    showToast(`Exported to ${dest}`);
  } catch (e) {
    showToast('Export canceled or failed');
  }
});

// ── Import modal ──────────────────────────────────────────────────────────

// Whether an import is actively running in the backend right now — lets
// Cancel tell "stop a running import" apart from "just close this modal."
let importRunning = false;

function openImportModal() {
  document.getElementById('importModal').classList.add('open');
  // If an import is still running in the background (the user closed the
  // modal on it earlier rather than canceling), reflect that truthfully
  // instead of showing "Choose a folder to import" next to a button that's
  // still disabled and would silently do nothing if clicked.
  if (!importRunning) {
    document.getElementById('importStatus').textContent = 'Choose a folder to import';
    setImportProgress(0, 0);
  }
}
function closeImportModal() { document.getElementById('importModal').classList.remove('open'); }
document.getElementById('railImportBtn').addEventListener('click', openImportModal);
document.getElementById('topbarImportBtn').addEventListener('click', openImportModal);

// ── Bulk tag correction (ML-SPEC.md §5) — the grid multi-select
// primitive's first real use. Uses the same promptTagNameInput helper the
// drawer's own tag-add flow does (defined next to renderDrawerTags,
// above), rather than a native prompt() (which this app never uses for
// tag entry either).

document.getElementById('bulkAddTagBtn').addEventListener('click', (e) => {
  const ids = [...bulkSelection];
  promptTagNameInput(e.currentTarget, async (tag) => {
    try {
      await invoke('bulk_add_tag', { imageIds: ids, tag });
    } catch (err) {
      // bulk_add_tag's own contract (src-tauri/src/lib.rs): stops at the
      // first failing id, ids processed before it stay applied. Which
      // ones isn't knowable from here, so — matching get_image_detail's
      // own catch (this file, ~line 518): don't optimistically apply the
      // change to any cached item on failure, rather than guess and risk
      // the grid showing a tag that was never actually saved.
      showToast('Could not add the tag to every selected photo');
      return;
    }
    for (const id of ids) {
      const item = [...itemCache.values()].find(it => it.id === id);
      if (item && !item.tags.includes(tag)) item.tags = [...item.tags, tag].sort();
    }
    scheduleRenderWindow();
    renderTagPop();
  });
});

document.getElementById('bulkRemoveTagBtn').addEventListener('click', (e) => {
  const ids = [...bulkSelection];
  promptTagNameInput(e.currentTarget, async (tag) => {
    try {
      await invoke('bulk_remove_tag', { imageIds: ids, tag });
    } catch (err) {
      // See bulkAddTagBtn's matching catch above for why this returns
      // early instead of optimistically updating the cache.
      showToast('Could not remove the tag from every selected photo');
      return;
    }
    for (const id of ids) {
      const item = [...itemCache.values()].find(it => it.id === id);
      if (item) item.tags = item.tags.filter(t => t !== tag);
    }
    scheduleRenderWindow();
    renderTagPop();
  });
});

document.getElementById('bulkClearBtn').addEventListener('click', clearBulkSelection);

document.getElementById('importCancelBtn').addEventListener('click', () => {
  if (importRunning) {
    // Fire-and-forget: `cancel_import` just sets a flag `import_directory`'s
    // loop checks after its current file finishes, so this doesn't block —
    // the in-flight invoke('import_directory') call's own `finally` handles
    // cleanup (re-enabling the Choose Folder button, resetting progress)
    // once the backend actually winds down, whether or not this modal is
    // still open to see it happen.
    invoke('cancel_import').catch(() => {});
    document.getElementById('importStatus').textContent = 'Canceling…';
  }
  closeImportModal();
});

function setImportProgress(current, total) {
  const box = document.getElementById('importProgress');
  const fill = document.getElementById('importProgressFill');
  const label = document.getElementById('importProgressLabel');
  box.style.display = total > 0 ? 'block' : 'none';
  fill.style.width = total > 0 ? `${Math.min(100, (current / total) * 100)}%` : '0%';
  label.textContent = total > 0 ? `${current} of ${total}` : '';
}

document.getElementById('importChooseBtn').addEventListener('click', async () => {
  const status = document.getElementById('importStatus');
  const chooseBtn = document.getElementById('importChooseBtn');
  status.textContent = 'Choose a folder…';
  setImportProgress(0, 0);
  chooseBtn.disabled = true;

  let unlisten = null;
  try {
    // `import_directory` reports its own progress via the `import-progress`
    // event (backend counts the folder up front, since its own file walk is
    // lazy and doesn't know a total until it's done) rather than a return
    // value, since the command doesn't resolve until the whole import
    // finishes. Setting this up inside the try means a failure here still
    // hits `finally` and re-enables the button, instead of leaving it
    // permanently disabled.
    unlisten = await listen('import-progress', (event) => {
      const { current, total } = event.payload;
      status.textContent = `Importing…`;
      setImportProgress(current, total);
    });
    importRunning = true;
    const { imported, canceled } = await invoke('import_directory');
    if (canceled) {
      showToast(`Import canceled — ${imported} photo${imported === 1 ? '' : 's'} imported so far`);
    } else {
      showToast(`Imported ${imported} photo${imported === 1 ? '' : 's'}`);
    }
    closeImportModal();
    refreshGrid();
    refreshReviewBadge();
    refreshPeopleBadge();
  } catch (e) {
    // Backend errors (CmdError) serialize as plain strings — surface the
    // real one (e.g. "an import is already in progress") rather than a
    // generic message that would mask it.
    status.textContent = typeof e === 'string' ? e : 'Import canceled or failed.';
  } finally {
    importRunning = false;
    if (unlisten) unlisten();
    chooseBtn.disabled = false;
    setImportProgress(0, 0);
  }
});

// ── Review queue ──────────────────────────────────────────────────────────
async function refreshReviewBadge() {
  let entries = [];
  try { entries = await invoke('list_review_queue'); } catch (e) { console.error(e); }
  const badge = document.getElementById('reviewBadge');
  if (entries.length) { badge.style.display = 'flex'; badge.textContent = entries.length; }
  else badge.style.display = 'none';
  document.getElementById('reviewCount').textContent = `${entries.length} pending`;
  return entries;
}

async function renderReviewQueue() {
  const entries = await refreshReviewBadge();
  const el = document.getElementById('review-pairs');
  if (!entries.length) {
    el.innerHTML = `<div class="empty-results" style="position:static">Nothing to review right now.</div>`;
    return;
  }
  el.innerHTML = entries.map((entry, idx) => `
    <div class="review-pair">
      <div class="review-pair-head" data-idx="${idx}">
        <div>
          <div class="review-pair-title">Image ${entry.imageA.id} &nbsp;≈&nbsp; Image ${entry.imageB.id}</div>
          <div class="review-pair-sub">${fmtDate(entry.imageA.captureDate)}</div>
        </div>
        <span class="hamming-badge">${entry.hammingDistance}-bit diff</span>
      </div>
      <div class="review-compare" id="compare-${idx}" style="display:none">
        <div class="review-candidate suggested">
          <div class="suggested-badge">SUGGESTED KEEPER</div>
          <div class="review-candidate-img"><img src="${assetSrc(entry.imageA.thumbnailPath)}" alt=""></div>
          <div class="candidate-meta">${fmtDate(entry.imageA.captureDate)}<br>${entry.imageA.tags.map(escapeHtml).join(', ') || 'no tags'}</div>
        </div>
        <div class="review-candidate">
          <div class="review-candidate-img"><img src="${assetSrc(entry.imageB.thumbnailPath)}" alt=""></div>
          <div class="candidate-meta">${fmtDate(entry.imageB.captureDate)}<br>${entry.imageB.tags.map(escapeHtml).join(', ') || 'no tags'}</div>
        </div>
      </div>
      <div class="review-actions" id="actions-${idx}" style="display:none">
        <button class="btn" data-dismiss="${entry.queueId}">Keep both</button>
        <button class="btn btn-primary" data-merge="${entry.queueId}" data-keeper="${entry.imageA.id}">Merge — tags combine, quarantine the other</button>
      </div>
    </div>
  `).join('');

  el.querySelectorAll('.review-pair-head').forEach(head => head.addEventListener('click', () => {
    const idx = head.dataset.idx;
    const c = document.getElementById('compare-' + idx), a = document.getElementById('actions-' + idx);
    const open = c.style.display !== 'none';
    c.style.display = open ? 'none' : 'grid';
    a.style.display = open ? 'none' : 'flex';
  }));
  el.querySelectorAll('[data-dismiss]').forEach(btn => btn.addEventListener('click', async () => {
    await invoke('resolve_review_pair', { queueId: Number(btn.dataset.dismiss), action: 'dismiss', keeperId: null });
    showToast('Kept both');
    renderReviewQueue();
  }));
  el.querySelectorAll('[data-merge]').forEach(btn => btn.addEventListener('click', async () => {
    await invoke('resolve_review_pair', { queueId: Number(btn.dataset.merge), action: 'merge', keeperId: Number(btn.dataset.keeper) });
    showToast('Merged');
    renderReviewQueue();
    refreshGrid();
  }));
}

// ── People view (ML-SPEC.md §6, ticket 028, Milestone ML-4 Slice C/D1) ─────
async function refreshPeopleBadge() {
  let count = 0;
  try { count = await invoke('pending_face_review_count'); } catch (e) { console.error(e); }
  const badge = document.getElementById('peopleBadge');
  if (count) { badge.style.display = 'flex'; badge.textContent = count; }
  else badge.style.display = 'none';
  return count;
}

// Rebuilds the shared `<datalist>` naming autocomplete draws from — one
// list refreshed whenever the People view (re)renders, not per-card, since
// every card's naming input needs the exact same option set.
function renderPersonDatalist(persons) {
  let list = document.getElementById('personNames');
  if (!list) {
    list = document.createElement('datalist');
    list.id = 'personNames';
    document.body.appendChild(list);
  }
  list.innerHTML = persons.map(p => `<option value="${escapeHtml(p.name)}"></option>`).join('');
}

// The §6-tier-2 "is this also Alice?" queue (028 decision #2, Slice D1) —
// a compact single-photo card per entry, distinct from dedupe's
// side-by-side comparison shape (that's Merge's shape, Slice D2): there's
// only one candidate photo and one yes/no question here, not two images
// to pick a keeper between.
async function renderPeopleNeedsReview() {
  let entries = [];
  try { entries = await invoke('list_pending_face_matches'); } catch (e) { console.error(e); }
  const el = document.getElementById('people-needs-review');
  el.innerHTML = entries.map(m => `
    <div class="face-match-card">
      <div class="face-match-thumb">${m.cropThumbnailPath ? `<img src="${assetSrc(m.cropThumbnailPath)}" alt="">` : ''}</div>
      <div>
        <div class="face-match-question">Is this also <b>${escapeHtml(m.suggestedPersonName)}</b>?</div>
        <div class="face-match-similarity">${Math.round(m.similarityScore * 100)}% similar</div>
      </div>
      <div class="face-match-actions">
        <button class="btn" data-dismiss-match="${m.queueId}">No</button>
        <button class="btn btn-primary" data-confirm-match="${m.queueId}">Yes</button>
      </div>
    </div>
  `).join('');

  el.querySelectorAll('[data-confirm-match]').forEach(btn => btn.addEventListener('click', async () => {
    try {
      await invoke('confirm_face_match', { queueId: Number(btn.dataset.confirmMatch) });
      renderPeopleView();
    } catch (err) { showToast('Could not confirm this match'); }
  }));
  el.querySelectorAll('[data-dismiss-match]').forEach(btn => btn.addEventListener('click', async () => {
    try {
      await invoke('dismiss_face_match', { queueId: Number(btn.dataset.dismissMatch) });
      renderPeopleView();
    } catch (err) { showToast('Could not dismiss this match'); }
  }));
}

async function renderPeopleView() {
  await refreshPeopleBadge();
  await renderPeopleNeedsReview();
  let clusters = [], persons = [];
  try {
    [clusters, persons] = await Promise.all([
      invoke('list_face_clusters', { includeHidden: false }),
      invoke('list_persons'),
    ]);
  } catch (e) { showToast('Could not load People'); return; }

  renderPersonDatalist(persons);
  document.getElementById('peopleCount').textContent = `${clusters.length} group${clusters.length === 1 ? '' : 's'}`;

  const grid = document.getElementById('people-grid');
  if (!clusters.length) {
    grid.innerHTML = `<div class="empty-results" style="position:static">No faces grouped yet.</div>`;
    return;
  }
  grid.innerHTML = clusters.map(c => `
    <div class="people-card" data-cluster-card="${c.id}">
      <div class="people-card-thumb" data-expand-cluster="${c.id}">${c.representativeCropPath ? `<img src="${assetSrc(c.representativeCropPath)}" alt="">` : ''}</div>
      <div class="people-card-crops" id="crops-${c.id}" style="display:none"></div>
      <div class="people-card-body">
        <div class="people-card-count">${c.photoCount} photo${c.photoCount === 1 ? '' : 's'}</div>
        ${c.personName
          ? `<div class="people-card-name">${escapeHtml(c.personName)}</div>`
          : `<button class="tag-add" data-name-cluster="${c.id}">+ Name this person</button>`}
        <button class="people-card-hide-btn" data-hide-cluster="${c.id}">Hide</button>
      </div>
    </div>
  `).join('');

  // "click a cluster, see its member thumbnails + photo count" (028
  // decision #3) — a read-only expand, fetched lazily on first click
  // rather than upfront for every card. Not the selectable grid Slice D's
  // split/merge flow needs; this is display only.
  grid.querySelectorAll('[data-expand-cluster]').forEach(thumb => thumb.addEventListener('click', async (e) => {
    e.stopPropagation();
    const panel = document.getElementById(`crops-${thumb.dataset.expandCluster}`);
    const open = panel.style.display !== 'none';
    if (open) { panel.style.display = 'none'; return; }
    if (!panel.dataset.loaded) {
      let crops = [];
      try { crops = await invoke('list_cluster_face_crops', { clusterId: Number(thumb.dataset.expandCluster) }); } catch (err) { showToast('Could not load this group’s faces'); return; }
      panel.innerHTML = crops.map(p => `<img src="${assetSrc(p)}" alt="">`).join('') || `<span style="color:var(--text-faint);font-size:11px">No face crops yet</span>`;
      panel.dataset.loaded = '1';
    }
    panel.style.display = 'flex';
  }));

  grid.querySelectorAll('button[data-name-cluster]').forEach(btn => btn.addEventListener('click', (e) => {
    e.stopPropagation();
    promptTagNameInput(btn, async (value) => {
      try {
        await invoke('name_face_cluster', { clusterId: Number(btn.dataset.nameCluster), personName: value });
        renderPeopleView();
      } catch (err) { showToast('Could not name this person'); }
    }, { placeholder: 'person’s name…', listId: 'personNames' });
  }));
  grid.querySelectorAll('button[data-hide-cluster]').forEach(btn => btn.addEventListener('click', async (e) => {
    e.stopPropagation();
    try {
      await invoke('set_face_cluster_hidden', { clusterId: Number(btn.dataset.hideCluster), hidden: true });
      showToast('Hidden — hidden groups are never deleted');
      renderPeopleView();
    } catch (err) { showToast('Could not hide this group'); }
  }));
}

// ── Nav switching ─────────────────────────────────────────────────────────
let currentView = 'grid';

// The nav rail's own click handler and the drawer's "jump to the People
// view" chip links (`renderDrawerPeople`) both need this exact
// switch-tab/toggle-panels/re-render sequence — factored out once rather
// than the drawer simulating a rail-btn click to reach it.
function switchView(name) {
  document.querySelectorAll('.rail-btn[data-view]').forEach(b => b.classList.toggle('active', b.dataset.view === name));
  currentView = name;
  document.getElementById('view-grid').style.display = currentView === 'grid' ? 'flex' : 'none';
  document.getElementById('view-review').style.display = currentView === 'review' ? 'flex' : 'none';
  document.getElementById('view-people').style.display = currentView === 'people' ? 'flex' : 'none';
  if (currentView === 'grid') layout();
  if (currentView === 'review') renderReviewQueue();
  if (currentView === 'people') renderPeopleView();
}

document.querySelectorAll('.rail-btn[data-view]').forEach(btn => {
  btn.addEventListener('click', () => switchView(btn.dataset.view));
});

// Scrolls to and briefly highlights one cluster's card in the People view
// — the "jump to that person's cluster" half of a drawer chip click (028
// decision #5). Cards are rendered async by `renderPeopleView`, so this
// polls briefly for the card to exist rather than assuming it's already
// in the DOM the instant `switchView` returns.
async function jumpToCluster(clusterId) {
  switchView('people');
  for (let attempt = 0; attempt < 20; attempt++) {
    const card = document.querySelector(`.people-card[data-cluster-card="${clusterId}"]`);
    if (card) {
      card.classList.add('people-card-highlight');
      setTimeout(() => card.classList.remove('people-card-highlight'), 1600);
      card.scrollIntoView({ block: 'center' });
      return;
    }
    await new Promise(r => setTimeout(r, 25));
  }
}
document.getElementById('view-grid').style.display = 'flex';
document.getElementById('view-grid').style.flexDirection = 'column';
document.getElementById('view-grid').style.flex = '1';
document.getElementById('view-grid').style.minHeight = '0';
// view-review ships with a static `style="display:none"` attribute in
// index.html, but on at least one real WebView2 install that attribute's
// text was present (readable via getAttribute) without ever being parsed
// into the live CSSOM (element.style.display read back empty, and the
// panel rendered visibly stacked below the grid). Don't rely on the static
// attribute for initial visibility — set it explicitly at boot, the same
// way view-grid's state is already established above.
document.getElementById('view-review').style.display = 'none';
document.getElementById('view-people').style.display = 'none';

// ── First-run vault setup (Milestone 5.5) ────────────────────────────────
//
// Ports workplan/design/lenslocker-design.html's #firstrun screen
// (owner-approved) onto the real backend: `pick_library_folder` opens the
// native folder dialog (tauri-plugin-dialog, no default/pre-filled path,
// matching the design), `inspect_library_folder` replaces the design's
// FAKE_CHOICES map with a real existing-catalog check and real free-space
// number, and `create_library`/`open_existing_library` replace the design's
// "just flip to the main app" simulation with an actual catalog swap.
let firstrunChoice = null; // { path, existingLibrary, freeBytes } | null

function showFirstRun(previousPathUnreachable) {
  document.getElementById('firstrun').classList.remove('hidden');
  document.getElementById('mainApp').classList.add('hidden');
  const banner = document.getElementById('firstrunUnreachable');
  if (previousPathUnreachable) {
    document.getElementById('firstrunUnreachableText').textContent =
      `Your previous vault at ${previousPathUnreachable} could not be found — it may be on a drive that's not connected. Choose a location to continue.`;
    banner.style.display = 'flex';
  } else {
    banner.style.display = 'none';
  }
}

function showMainApp() {
  document.getElementById('firstrun').classList.add('hidden');
  document.getElementById('mainApp').classList.remove('hidden');
}

async function chooseFolder() {
  let path;
  try { path = await invoke('pick_library_folder'); } catch (e) { return; }
  if (!path) return; // user canceled

  let inspected;
  try { inspected = await invoke('inspect_library_folder', { path }); } catch (e) {
    showToast('Could not read that folder');
    return;
  }
  firstrunChoice = { path, existingLibrary: inspected.existingLibrary, freeBytes: inspected.freeBytes };
  renderFirstrunChoice();
}
document.getElementById('chooseFolderBtn').addEventListener('click', chooseFolder);

function renderFirstrunChoice() {
  const box = document.getElementById('pickerBox');
  const existingBanner = document.getElementById('firstrunExisting');
  const newOptions = document.getElementById('firstrunNewOptions');
  const confirmBtn = document.getElementById('firstrunConfirmBtn');

  box.classList.add('chosen');
  // No fixed "low space" threshold ships in the design beyond an
  // illustrative fake value — 10 GB is a reasonable, clearly-documented
  // floor for "this fills up fast" on a photo library.
  const lowSpace = firstrunChoice.freeBytes < 10_000_000_000;
  const spaceClass = lowSpace ? 'space-warn' : 'space-ok';
  const spaceNote = lowSpace
    ? `⚠ only ${fmtGb(firstrunChoice.freeBytes)} GB free — this fills up fast`
    : `${fmtGb(firstrunChoice.freeBytes)} GB free`;
  box.innerHTML = `
    <div class="picker-chosen-row">
      <div class="picker-chosen-icon">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v9a2 2 0 01-2 2H5a2 2 0 01-2-2V7z"/></svg>
      </div>
      <div style="min-width:0; flex:1">
        <div class="picker-chosen-path">${escapeHtml(firstrunChoice.path)}</div>
        <div class="picker-chosen-meta"><span class="${spaceClass}">${spaceNote}</span></div>
      </div>
      <button class="picker-change-btn" id="changeFolderBtn">Change</button>
    </div>
  `;
  document.getElementById('changeFolderBtn').addEventListener('click', resetFirstrunPicker);

  existingBanner.style.display = firstrunChoice.existingLibrary ? 'flex' : 'none';
  newOptions.style.display = firstrunChoice.existingLibrary ? 'none' : 'block';

  confirmBtn.disabled = false;
  confirmBtn.textContent = firstrunChoice.existingLibrary ? 'Open Vault' : 'Create Vault';
}

function resetFirstrunPicker() {
  firstrunChoice = null;
  const box = document.getElementById('pickerBox');
  box.classList.remove('chosen');
  box.innerHTML = `
    <svg class="picker-empty-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5"><path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v9a2 2 0 01-2 2H5a2 2 0 01-2-2V7z"/></svg>
    <span class="picker-empty-text">No folder chosen yet</span>
    <button class="btn" id="chooseFolderBtn">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v9a2 2 0 01-2 2H5a2 2 0 01-2-2V7z"/></svg>
      Choose Folder…
    </button>
  `;
  // The innerHTML replace above tore out the original button's listener —
  // re-bind to the same handler.
  box.querySelector('#chooseFolderBtn').addEventListener('click', chooseFolder);
  document.getElementById('firstrunExisting').style.display = 'none';
  document.getElementById('firstrunNewOptions').style.display = 'none';
  document.getElementById('firstrunConfirmBtn').disabled = true;
  document.getElementById('firstrunConfirmBtn').textContent = 'Choose a folder first';
}

document.getElementById('conversionToggle').addEventListener('click', () => {
  document.getElementById('conversionToggle').classList.toggle('on');
});

document.getElementById('firstrunConfirmBtn').addEventListener('click', async () => {
  if (!firstrunChoice) return;
  const btn = document.getElementById('firstrunConfirmBtn');
  btn.disabled = true;
  const verb = firstrunChoice.existingLibrary ? 'Opening' : 'Creating';
  btn.textContent = `${verb}…`;
  try {
    if (firstrunChoice.existingLibrary) {
      await invoke('open_existing_library', { path: firstrunChoice.path });
    } else {
      const conversionEnabled = document.getElementById('conversionToggle').classList.contains('on');
      await invoke('create_library', { path: firstrunChoice.path, conversionEnabled });
    }
  } catch (e) {
    showToast('Could not set up the vault at that location');
    btn.disabled = false;
    btn.textContent = firstrunChoice.existingLibrary ? 'Open Vault' : 'Create Vault';
    return;
  }
  showMainApp();
  const doneVerb = firstrunChoice.existingLibrary ? 'Opened' : 'Created';
  showToast(`${doneVerb} vault at ${firstrunChoice.path}`);
  startMainApp();
});

// ── Settings (Milestone 5.5) ──────────────────────────────────────────────
// hamming_threshold/retention_days were both decided as user-tunable
// (tickets 011, 005) but never given a UI until now — a minimal modal
// reusing the existing .modal-scrim pattern.
function openSettingsModal() {
  document.getElementById('settingsModal').classList.add('open');
  invoke('get_app_settings').then(s => {
    document.getElementById('settingsHammingInput').value = s.hammingThreshold;
    document.getElementById('settingsRetentionInput').value = s.retentionDays;
  }).catch(() => showToast('Could not load settings'));
}
function closeSettingsModal() { document.getElementById('settingsModal').classList.remove('open'); }
document.getElementById('railSettingsBtn').addEventListener('click', openSettingsModal);
document.getElementById('settingsCancelBtn').addEventListener('click', closeSettingsModal);
document.getElementById('settingsSaveBtn').addEventListener('click', async () => {
  const hammingThreshold = Number(document.getElementById('settingsHammingInput').value);
  const retentionDays = Number(document.getElementById('settingsRetentionInput').value);
  try {
    await invoke('update_app_settings', { hammingThreshold, retentionDays });
    showToast('Settings saved');
    closeSettingsModal();
  } catch (e) {
    showToast('Could not save settings');
  }
});

// ── Boot ───────────────────────────────────────────────────────────────
// `check_library_status` decides between the first-run screen (true first
// run, or a previously-configured library whose path is no longer
// reachable — an unplugged external drive, say) and the main app. Only
// once a live library exists do we call anything that touches the
// catalog — list_images/list_review_queue/etc. would otherwise error.
renderSortPop();

function startMainApp() {
  refresh();
  refreshReviewBadge();
  refreshPeopleBadge();
  // Populated here too, not only on first People-view visit — the
  // drawer's inline-naming chip (`renderDrawerPeople`) needs the
  // autocomplete datalist to already exist even if the user never opens
  // the People view first.
  invoke('list_persons').then(renderPersonDatalist).catch(() => {});
}

async function boot() {
  let status;
  try {
    status = await invoke('check_library_status');
  } catch (e) {
    console.error(e);
    status = { ready: false, previousPathUnreachable: null };
  }
  if (status.ready) {
    showMainApp();
    startMainApp();
  } else {
    showFirstRun(status.previousPathUnreachable);
  }
}
boot();
