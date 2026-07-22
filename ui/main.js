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
  persons: new Set(),        // person ids (ticket 031's Person facet, Milestone ML-5)
  sort: 'captured-desc',
  query: '',                 // keyword (FTS) search box text — irrelevant while similarityQuery is set
  searchByMeaning: false,    // ML-SPEC.md §8: the search box's mode toggle, keyword vs. semantic
  similarityQuery: null,     // {type:'image', imageId} | {type:'text', text} | null — §8's "Find Similar"/text-to-image search
};

// id -> name, refreshed whenever the Person popover renders — active-filter
// pills and the popover itself both need a name for an id, and list_persons
// is the only source of that mapping.
let personNamesById = new Map();

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
    persons: [...state.persons],
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

// ── Multi-select primitive (ML-SPEC.md §5/§6) ────────────────────────────
// "reuses one shared multi-select primitive that §6's face-cluster
// splitting also needs... build it generically enough to serve both call
// sites, not narrowly scoped to faces." One generic Set-backed selection
// manager, created fresh by each call site (the grid's bulk tag
// correction below; the People view's per-cluster crop picker for Split).
// Owns membership tracking and mutation only — not rendering, since the
// grid (re-renders its whole visible window) and a crop panel (re-renders
// one detached subtree) sync completely different DOM shapes; each call
// site supplies its own `onChange` to do that.
function createMultiSelect(onChange) {
  const selection = new Set();
  const notify = () => onChange?.();
  return {
    selection,
    has: (id) => selection.has(id),
    get size() { return selection.size; },
    toggle(id) {
      selection.has(id) ? selection.delete(id) : selection.add(id);
      notify();
    },
    addRange(ids) {
      ids.forEach(id => selection.add(id));
      notify();
    },
    clear() {
      selection.clear();
      notify();
    },
  };
}

// Interaction design (checkbox-on-hover + shift-range, a bulk bar once
// 1+ selected) is this build's own judgment call — ML-SPEC.md deliberately
// specifies *that* a multi-select primitive is needed, not its exact
// mechanics. Follows the common photo-app convention (Google/Apple
// Photos): once 1+ items are selected, clicking a thumb's body continues
// selecting instead of opening the drawer, so a half-selected state can't
// accidentally be abandoned by a stray click.
const bulkSelect = createMultiSelect(() => { updateBulkBar(); scheduleRenderWindow(); }); // image ids, not indices (ids survive re-sorts/scrolls; indices don't)
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

// Routes to the plain filtered/sorted query, or one of §8's two
// similarity queries when state.similarityQuery is set — "Find Similar"/
// "search by meaning" reuse the same grid/pagination machinery as normal
// browsing ("same grid, a new sort order... no new rendering surface"),
// just a different data source. The ordinary filter facets still compose
// either way (ticket 031's reuse pattern extends to similarity search).
function fetchGridPage(offset) {
  if (!state.similarityQuery) {
    return invoke('list_images', { filters: filtersDto(), sort: state.sort, search: state.query || null, offset, limit: PAGE });
  }
  if (state.similarityQuery.type === 'image') {
    return invoke('find_similar_images', { imageId: state.similarityQuery.imageId, filters: filtersDto(), offset, limit: PAGE });
  }
  return invoke('search_by_text', { query: state.similarityQuery.text, filters: filtersDto(), offset, limit: PAGE });
}

async function fetchPage(offset, token) {
  if (pendingPages.has(offset)) return;
  pendingPages.add(offset);
  try {
    const res = await fetchGridPage(offset);
    if (token !== requestToken) return; // stale — a newer filter/sort/search superseded this request
    total = res.total;
    res.items.forEach((item, i) => itemCache.set(offset + i, item));
    layout();
  } catch (e) {
    console.error('grid page fetch failed', e);
    // A similarity query can fail for a real, expected reason today (the
    // source photo hasn't been analyzed yet — nothing populates
    // embeddings automatically until Milestone ML-6) — worth surfacing,
    // unlike a normal list_images failure the console log already covers.
    if (state.similarityQuery) showToast(typeof e === 'string' ? e : 'Could not run this search');
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
  const picked = bulkSelect.has(item.id);
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
  // (module doc comment above bulkSelect's declaration) — a stray
  // click can't silently abandon a half-built selection. Otherwise, a
  // body click opens the drawer as it always has.
  if (onPickTarget || bulkSelect.size > 0) {
    if (onPickTarget) e.stopPropagation();
    if (e.shiftKey && lastClickedIdx !== null) {
      selectRange(lastClickedIdx, idx);
    } else {
      bulkSelect.toggle(id);
    }
    lastClickedIdx = idx;
    return;
  }

  openDrawer(id);
});

// Selects every *currently loaded* item between fromIdx and toIdx
// (inclusive) — a disclosed limitation, not a bug: indices outside what's
// been scrolled-to/fetched yet (this grid only loads its visible window +
// buffer, per the file's own top-of-file virtualization note) can't be
// included in a range that was never fetched.
function selectRange(fromIdx, toIdx) {
  const [lo, hi] = fromIdx <= toIdx ? [fromIdx, toIdx] : [toIdx, fromIdx];
  const ids = [];
  for (let i = lo; i <= hi; i++) {
    const item = itemCache.get(i);
    if (item) ids.push(item.id);
  }
  bulkSelect.addRange(ids);
}

function clearBulkSelection() {
  bulkSelect.clear();
  lastClickedIdx = null;
}

function updateBulkBar() {
  const bar = document.getElementById('bulkBar');
  if (!bar) return;
  bar.style.display = bulkSelect.size > 0 ? 'flex' : 'none';
  const countEl = document.getElementById('bulkCount');
  if (countEl) countEl.textContent = `${bulkSelect.size} selected`;
}

gridWrap.addEventListener('scroll', scheduleRenderWindow);
window.addEventListener('resize', () => layout());

function hasActiveQuery() {
  return !!(state.dateRange || state.formats.size || state.sources.size || state.tags.size || state.persons.size || state.query);
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
  const dateOn = !!state.dateRange, fmtOn = state.formats.size > 0, srcOn = state.sources.size > 0, tagOn = state.tags.size > 0, personOn = state.persons.size > 0;
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
    <div class="chip ${personOn ? 'on' : ''}" id="personChip">
      Person${personOn ? ` (${state.persons.size})` : ''}
      <div class="popover" id="personPop"></div>
    </div>
  `;
  document.getElementById('dateChip').addEventListener('click', (e) => togglePop('datePop', e));
  document.getElementById('fmtChip').addEventListener('click', (e) => togglePop('fmtPop', e));
  document.getElementById('srcChip').addEventListener('click', (e) => togglePop('srcPop', e));
  document.getElementById('tagChip').addEventListener('click', (e) => togglePop('tagPop', e));
  document.getElementById('personChip').addEventListener('click', (e) => togglePop('personPop', e));
  [...bar.querySelectorAll('.popover')].forEach(p => p.addEventListener('click', (e) => e.stopPropagation()));

  renderDatePop();
  await Promise.all([renderFormatPop(), renderSourcePop(), renderTagPop(), renderPersonPop()]);
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
    return `<div class="popover-item ${on ? 'checked' : ''}" data-tag="${escapeHtml(t.name)}">
      <span class="box">${on ? checkIcon() : ''}</span>
      <span class="label">${escapeHtml(t.name)}</span>
      <button class="popover-rename" data-rename-tag="${escapeHtml(t.name)}" title="Fix this tag's name">✎</button>
      <button class="popover-rename popover-delete" data-delete-tag="${escapeHtml(t.name)}" title="Delete this tag entirely">🗑</button>
      <span class="n">${t.count}</span>
    </div>`;
  }).join('');
  pop.querySelectorAll('[data-tag]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    const t = el.dataset.tag;
    state.tags.has(t) ? state.tags.delete(t) : state.tags.add(t);
    refresh();
  }));

  // Fixes an invalidly-tagged name globally, not just on one photo —
  // renames the tag row itself, so every image already carrying it picks
  // up the correction (or merges into an existing tag of the corrected
  // name, if one exists — lenslocker_catalog::rename_tag's own doc
  // comment). `stopPropagation` keeps this from also toggling the filter
  // checkbox the parent `.popover-item` div listens for.
  pop.querySelectorAll('[data-rename-tag]').forEach(btn => btn.addEventListener('click', (e) => {
    e.stopPropagation();
    const oldName = btn.dataset.renameTag;
    promptTagNameInput(btn, async (newName) => {
      if (newName === oldName) return;
      try {
        await commitTagRename(oldName, newName);
      } catch (err) { showToast('Could not rename this tag'); }
    }, { placeholder: 'new tag name…', value: oldName });
  }));

  // A tag that shouldn't exist at all, not a misspelling of a real one —
  // rename_tag has no merge target for "just get rid of it".
  pop.querySelectorAll('[data-delete-tag]').forEach(btn => btn.addEventListener('click', async (e) => {
    e.stopPropagation();
    const name = btn.dataset.deleteTag;
    try {
      await invoke('delete_tag', { name });
      state.tags.delete(name);
      showToast(`Deleted tag "${name}"`);
      renderTagPop();
      refresh();
    } catch (err) { showToast('Could not delete this tag'); }
  }));
}

// Factored out of renderTagPop's click handler so the active-filter Set
// (keyed by tag name, not id — state.tags has no other identifier to key
// on) stays consistent with whatever the tag is called after the rename;
// otherwise a currently-filtered tag would silently stop matching anything
// the moment it's renamed out from under the filter.
async function commitTagRename(oldName, newName) {
  await invoke('rename_tag', { old: oldName, new: newName });
  if (state.tags.has(oldName)) { state.tags.delete(oldName); state.tags.add(newName); }
  showToast(`Renamed to "${newName}"`);
  renderTagPop();
  refresh();
}

// The Person facet (ticket 031, Milestone ML-5) — mirrors renderTagPop
// exactly, backed by list_persons (already built for the People view)
// instead of list_tags. personNamesById is refreshed here so active-filter
// pills (which only have an id, not a name) can look one up.
async function renderPersonPop() {
  const pop = document.getElementById('personPop');
  let persons = [];
  try { persons = await invoke('list_persons'); } catch (e) { console.error(e); }
  personNamesById = new Map(persons.map(p => [p.id, p.name]));
  pop.innerHTML = persons.map(p => {
    const on = state.persons.has(p.id);
    return `<div class="popover-item ${on ? 'checked' : ''}" data-person="${p.id}"><span class="box">${on ? checkIcon() : ''}</span><span class="label">${escapeHtml(p.name)}</span></div>`;
  }).join('') || `<div style="font-size:10px;color:var(--text-faint);padding:4px 8px 6px">No named people yet</div>`;
  pop.querySelectorAll('[data-person]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    const id = Number(el.dataset.person);
    state.persons.has(id) ? state.persons.delete(id) : state.persons.add(id);
    refresh();
  }));
}

function renderActiveFilters() {
  const pills = [];
  if (state.dateRange) pills.push({ text: state.dateRange.label, clear: () => { state.dateRange = null; } });
  state.formats.forEach(f => pills.push({ text: f.toUpperCase(), clear: () => state.formats.delete(f) }));
  state.sources.forEach(s => pills.push({ text: s.split(/[\\/]/).pop(), clear: () => state.sources.delete(s) }));
  state.tags.forEach(t => pills.push({ text: '#' + t, clear: () => state.tags.delete(t) }));
  state.persons.forEach(id => pills.push({ text: personNamesById.get(id) ?? `Person #${id}`, clear: () => state.persons.delete(id) }));
  // §8's similarity query — shown as just another active-filter pill
  // (reusing the existing "chip + clear ×" pattern) rather than a
  // separate banner UI element, since it behaves exactly like one: a
  // narrowing of the grid with one clear way to remove it.
  if (state.similarityQuery) {
    const isText = state.similarityQuery.type === 'text';
    const text = isText ? `Meaning: “${state.similarityQuery.text}”` : 'Similar to this photo';
    pills.push({
      text,
      clear: () => {
        state.similarityQuery = null;
        if (isText) document.getElementById('searchInput').value = '';
      },
    });
  }

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
  state.dateRange = null; state.formats.clear(); state.sources.clear(); state.tags.clear(); state.persons.clear();
  if (state.similarityQuery?.type === 'text') document.getElementById('searchInput').value = '';
  state.similarityQuery = null;
  refresh();
}

// Shared by the sort popover's own click handler and `openSavedAlbum` —
// both need to set state.sort and keep the visible sortLabel text in sync
// with it; only the popover click additionally closes/re-renders the
// popover and refreshes the grid immediately.
function setSort(key) {
  state.sort = key;
  document.getElementById('sortLabel').textContent = (SORT_OPTIONS.find(o => o.key === key) ?? SORT_OPTIONS[0]).label;
}

function renderSortPop() {
  const pop = document.getElementById('sortPop');
  pop.innerHTML = SORT_OPTIONS.map(o => {
    const on = state.sort === o.key;
    return `<div class="popover-item ${on ? 'checked' : ''}" data-sort="${o.key}"><span class="box">${on ? checkIcon() : ''}</span><span class="label">${o.label}</span></div>`;
  }).join('');
  pop.querySelectorAll('[data-sort]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    setSort(el.dataset.sort);
    // Picking an explicit column sort only makes sense for plain
    // browsing — a similarity ranking has no "newest captured"/"largest
    // file" order of its own — so it exits similarity mode rather than
    // being silently ignored while similarityQuery stays set.
    state.similarityQuery = null;
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
  searchDebounce = setTimeout(() => {
    if (state.searchByMeaning) {
      state.similarityQuery = value ? { type: 'text', text: value } : null;
    } else {
      // Typing a keyword search always exits image-similarity mode too
      // ("Find Similar" results) — a stray leftover similarity query
      // would otherwise silently ignore whatever was just typed, since
      // fetchGridPage checks similarityQuery before the plain search.
      state.similarityQuery = null;
      state.query = value;
    }
    refreshGrid();
  }, 180);
});

// §8's search-mode toggle — keyword (FTS, unchanged) vs. semantic
// (SigLIP text search). Re-routes whatever's currently typed rather than
// requiring the user to retype it after switching modes.
document.getElementById('searchModeToggle').addEventListener('click', () => {
  state.searchByMeaning = !state.searchByMeaning;
  const toggle = document.getElementById('searchModeToggle');
  toggle.classList.toggle('on', state.searchByMeaning);
  const input = document.getElementById('searchInput');
  input.placeholder = state.searchByMeaning ? 'Describe what you’re looking for…' : 'Search tags, camera, filename…';
  const value = input.value.trim();
  if (state.searchByMeaning) {
    state.query = '';
    state.similarityQuery = value ? { type: 'text', text: value } : null;
  } else {
    state.similarityQuery = null;
    state.query = value;
  }
  refreshGrid();
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
function promptTagNameInput(triggerEl, onCommit, { placeholder = 'tag name…', listId = null, value = '' } = {}) {
  const input = document.createElement('input');
  input.className = 'tag-input';
  input.placeholder = placeholder;
  if (listId) input.setAttribute('list', listId);
  if (value) input.value = value;
  triggerEl.replaceWith(input);
  input.focus();
  if (value) input.select();
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

// "Find Similar" (ML-SPEC.md §8) — always starts from a library-resident
// image already open in the drawer, per the spec's own entry-point
// decision (no external-file-picker flow anywhere else in this app).
// Clears any keyword/meaning search in progress, same reasoning as the
// search box's own mode switch: a stray leftover query would otherwise
// silently coexist with the new similarity mode instead of being
// replaced by it.
document.getElementById('findSimilarBtn').addEventListener('click', (e) => {
  e.stopPropagation();
  if (!currentDetail) return;
  state.query = '';
  document.getElementById('searchInput').value = '';
  state.similarityQuery = { type: 'image', imageId: currentDetail.id };
  closeDrawer();
  switchView('grid');
  refresh();
});

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

// ── Smart albums (ML-SPEC.md §7, ticket 031, Milestone ML-5) ───────────────
// "Save as Smart Album" saves the *current* filter/sort/search combination
// under a name — "no separate query-builder UI... let any combination of
// existing facets be named and saved" (ticket 031 decision #2). The saved
// blob is exactly filtersDto() plus sort/search, matching saved_albums'
// own documented shape, so a new facet is automatically saveable without
// ever touching this function again.
document.getElementById('saveAlbumBtn').addEventListener('click', (e) => {
  promptTagNameInput(e.currentTarget, async (name) => {
    const filters = JSON.stringify({ ...filtersDto(), sort: state.sort, search: state.query });
    try {
      await invoke('save_album', { name, filters });
      showToast(`Saved “${name}”`);
    } catch (err) { showToast('Could not save this album'); }
  }, { placeholder: 'album name…' });
});

async function renderAlbumsView() {
  let albums = [];
  try { albums = await invoke('list_saved_albums'); } catch (e) { showToast('Could not load Albums'); return; }
  document.getElementById('albumsCount').textContent = `${albums.length} album${albums.length === 1 ? '' : 's'}`;

  const list = document.getElementById('albums-list');
  if (!albums.length) {
    list.innerHTML = `<div class="empty-results" style="position:static">No albums saved yet.</div>`;
    return;
  }
  list.innerHTML = albums.map(a => `
    <div class="face-match-card">
      <div>
        <div class="people-card-name">${escapeHtml(a.name)}</div>
        <div class="face-match-similarity">Saved ${a.createdAt ? a.createdAt.slice(0, 10) : ''}</div>
      </div>
      <div class="face-match-actions">
        <button class="btn" data-delete-album="${a.id}">Delete</button>
        <button class="btn btn-primary" data-open-album="${a.id}">Open</button>
      </div>
    </div>
  `).join('');

  list.querySelectorAll('[data-open-album]').forEach(btn => btn.addEventListener('click', () => {
    const album = albums.find(a => a.id === Number(btn.dataset.openAlbum));
    if (album) openSavedAlbum(album);
  }));
  list.querySelectorAll('[data-delete-album]').forEach(btn => btn.addEventListener('click', async () => {
    try {
      await invoke('delete_saved_album', { id: Number(btn.dataset.deleteAlbum) });
      renderAlbumsView();
    } catch (err) { showToast('Could not delete this album'); }
  }));
}

// Loading a saved album means replacing `state`'s filter/sort/search
// fields wholesale from its stored JSON (the same shape `filtersDto()` +
// sort + search produced when it was saved), then switching to the grid
// — "opening one shows the same grid with that album's filters
// pre-applied and editable" (ticket 031 decision #3). "Editable" needs no
// special handling here: once loaded, this is just the live `state` like
// any manually-built filter combination, freely changeable afterward.
function openSavedAlbum(album) {
  let parsed;
  try { parsed = JSON.parse(album.filters); } catch (e) { showToast('This album’s saved filters are corrupted'); return; }

  state.formats = new Set(parsed.formats || []);
  state.sources = new Set(parsed.sources || []);
  state.tags = new Set(parsed.tags || []);
  state.persons = new Set(parsed.persons || []);
  state.dateRange = (parsed.dateFrom || parsed.dateTo)
    ? { label: `${parsed.dateFrom || '…'} – ${parsed.dateTo || '…'}`, from: parsed.dateFrom || null, to: parsed.dateTo || null }
    : null;
  state.query = parsed.search || '';

  setSort(parsed.sort || 'captured-desc');
  document.getElementById('searchInput').value = state.query;
  renderSortPop();

  switchView('grid');
  refresh();
}

// ── Bulk tag correction (ML-SPEC.md §5) — the grid multi-select
// primitive's first real use. Uses the same promptTagNameInput helper the
// drawer's own tag-add flow does (defined next to renderDrawerTags,
// above), rather than a native prompt() (which this app never uses for
// tag entry either).

document.getElementById('bulkAddTagBtn').addEventListener('click', (e) => {
  const ids = [...bulkSelect.selection];
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
  const ids = [...bulkSelect.selection];
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

// Renders one cluster grid (either the main, named-people grid or the
// collapsed "Unreviewed groups" grid — same card shape, same wiring)
// into `grid` and wires up its cards' expand/pick/name/hide interactions.
// Factored out of `renderPeopleView` so unnamed clusters can live in a
// separate, collapsed section (not mixed into the main People grid by
// default) without duplicating this template + event-wiring logic.
function renderClusterCards(grid, clusters) {
  if (!clusters.length) {
    grid.innerHTML = '';
    return;
  }
  grid.innerHTML = clusters.map(c => `
    <div class="people-card${mergeSelection.has(c.id) ? ' people-card-picked' : ''}" data-cluster-card="${c.id}">
      <div class="people-card-thumb" data-expand-cluster="${c.id}">
        ${c.representativeCropPath ? `<img src="${assetSrc(c.representativeCropPath)}" alt="">` : ''}
        <div class="people-card-pick" data-pick-cluster="${c.id}" title="Select to merge">${mergeSelection.has(c.id) ? checkIcon() : ''}</div>
      </div>
      <div class="people-card-crops" id="crops-${c.id}" style="display:none"></div>
      <div class="people-card-split-bar" id="split-${c.id}" style="display:none"></div>
      <div class="people-card-body">
        <div class="people-card-count">${c.photoCount} photo${c.photoCount === 1 ? '' : 's'}</div>
        ${c.personName
          ? `<button class="people-card-name" data-rename-person="${c.personId}" title="Fix this person's name">${escapeHtml(c.personName)}</button>`
          : `<button class="tag-add" data-name-cluster="${c.id}">+ Name this person</button>`}
        <div class="people-card-actions">
          ${c.personName ? `<button class="people-card-hide-btn" data-clear-cluster-name="${c.id}">Clear name</button>` : ''}
          <button class="people-card-hide-btn" data-hide-cluster="${c.id}">Hide</button>
        </div>
      </div>
    </div>
  `).join('');

  // "click a cluster, see its member thumbnails + photo count" (028
  // decision #3), and — since Slice D3 — the same crops are Split's
  // selection surface ("show every member face thumbnail... let the user
  // multi-select a subset"). Distinct from the merge checkbox above
  // (which operates on whole cluster cards): two different selection
  // granularities that only look similar at a glance. Crops are fetched
  // lazily on first expand and cached on the panel's dataset, since
  // toggling a selection re-renders the panel without a re-fetch.
  grid.querySelectorAll('[data-expand-cluster]').forEach(thumb => thumb.addEventListener('click', async (e) => {
    e.stopPropagation();
    const clusterId = Number(thumb.dataset.expandCluster);
    const panel = document.getElementById(`crops-${clusterId}`);
    const open = panel.style.display !== 'none';
    if (open) { panel.style.display = 'none'; return; }
    if (!panel.dataset.loaded) {
      let crops = [];
      try { crops = await invoke('list_cluster_face_crops', { clusterId }); } catch (err) { showToast('Could not load this group’s faces'); return; }
      panel.dataset.crops = JSON.stringify(crops);
      panel.dataset.loaded = '1';
    }
    renderCropsPanel(clusterId);
    panel.style.display = 'flex';
  }));

  grid.querySelectorAll('[data-pick-cluster]').forEach(pick => pick.addEventListener('click', (e) => {
    e.stopPropagation();
    toggleMergeSelection(Number(pick.dataset.pickCluster));
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
  // Fixes an invalidly-tagged person name — unlike data-name-cluster
  // above (which attaches *this one cluster* to whichever person the
  // typed name resolves to), this renames the person entity itself via
  // rename_person, so every cluster already attached to them picks up the
  // correction at once, not just the card being edited.
  grid.querySelectorAll('button[data-rename-person]').forEach(btn => btn.addEventListener('click', (e) => {
    e.stopPropagation();
    const personId = Number(btn.dataset.renamePerson);
    const currentName = btn.textContent;
    promptTagNameInput(btn, async (newName) => {
      if (newName === currentName) return;
      try {
        await invoke('rename_person', { personId, newName });
        showToast(`Renamed to "${newName}"`);
        renderPeopleView();
      } catch (err) { showToast('Could not rename this person'); }
    }, { placeholder: 'person’s name…', listId: 'personNames', value: currentName });
  }));
  // For a cluster attributed to a person entirely by mistake (not a typo
  // — that's data-rename-person above): reverts just this card back to
  // unidentified. Distinct from Hide, which keeps the name but stops
  // showing the card at all.
  grid.querySelectorAll('button[data-clear-cluster-name]').forEach(btn => btn.addEventListener('click', async (e) => {
    e.stopPropagation();
    try {
      await invoke('clear_face_cluster_name', { clusterId: Number(btn.dataset.clearClusterName) });
      showToast('Name cleared');
      renderPeopleView();
    } catch (err) { showToast('Could not clear this name'); }
  }));
  grid.querySelectorAll('button[data-hide-cluster]').forEach(btn => btn.addEventListener('click', async (e) => {
    e.stopPropagation();
    try {
      await invoke('set_face_cluster_hidden', { clusterId: Number(btn.dataset.hideCluster), hidden: true });
      showToast('Hidden — hidden groups are never deleted');
      // A hidden cluster drops out of the (non-hidden-only) list this view
      // shows — clear any stale merge selection referencing it, since the
      // merge bar's "2 selected" count must only ever count cards actually
      // visible and checked.
      mergeSelection.delete(Number(btn.dataset.hideCluster));
      updateMergeBar();
      renderPeopleView();
    } catch (err) { showToast('Could not hide this group'); }
  }));
}

async function renderPeopleView() {
  // Every crop-expand panel collapses back to closed on a fresh render
  // below (grid.innerHTML is rebuilt from scratch) — any split selection
  // would otherwise linger referencing a panel that no longer knows it
  // was ever expanded.
  splitSelections.clear();
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
  peopleClustersCache = clusters;

  // Unnamed clusters are backend bookkeeping, not "people" a user would
  // recognize — mixing them into the main grid (especially before the
  // face-alignment fix, when nearly every unnamed face collapsed into one
  // giant cluster) buried real named people in noise. They still need a
  // way to be named for the first time (the "Is this also X" review queue
  // only ever suggests matches against people who are *already* named), so
  // they move to a collapsed section instead of disappearing outright.
  const named = clusters.filter(c => c.personName);
  const unnamed = clusters.filter(c => !c.personName);

  document.getElementById('peopleCount').textContent = `${named.length} group${named.length === 1 ? '' : 's'}`;
  const namedGrid = document.getElementById('people-grid');
  if (named.length) {
    renderClusterCards(namedGrid, named);
  } else {
    namedGrid.innerHTML = `<div class="empty-results" style="position:static">No named people yet.</div>`;
  }

  const details = document.getElementById('people-unreviewed-details');
  document.getElementById('people-unreviewed-count').textContent = unnamed.length;
  details.style.display = unnamed.length ? '' : 'none';
  renderClusterCards(document.getElementById('people-unreviewed-grid'), unnamed);
}

// ── Cluster split (028 decision #4, Milestone ML-4 Slice D3) ───────────────
// One `createMultiSelect` instance per open cluster panel, not one shared
// instance — more than one crop-expand panel can be open at once (each
// People-view card expands independently), unlike the grid's single
// `bulkSelect`. Both still go through the same shared primitive above.
const splitSelections = new Map();

function getSplitSelect(clusterId) {
  if (!splitSelections.has(clusterId)) {
    splitSelections.set(clusterId, createMultiSelect(() => renderCropsPanel(clusterId)));
  }
  return splitSelections.get(clusterId);
}

// Renders one cluster's expanded crop grid from its cached `list_cluster_face_crops`
// response (`panel.dataset.crops`) — re-run on every selection toggle (via
// the multi-select's `onChange`), not just on first expand, since there's
// no fetch involved past that point.
function renderCropsPanel(clusterId) {
  const panel = document.getElementById(`crops-${clusterId}`);
  const crops = JSON.parse(panel.dataset.crops || '[]');
  const select = getSplitSelect(clusterId);

  panel.innerHTML = crops.length
    ? crops.map(crop => `
        <div class="face-crop-pick${select.has(crop.detectionId) ? ' face-crop-pick-selected' : ''}" data-crop-detection="${crop.detectionId}">
          <img src="${assetSrc(crop.cropThumbnailPath)}" alt="">
        </div>`).join('')
    : `<span style="color:var(--text-faint);font-size:11px">No face crops yet</span>`;

  panel.querySelectorAll('[data-crop-detection]').forEach(el => el.addEventListener('click', (e) => {
    e.stopPropagation();
    select.toggle(Number(el.dataset.cropDetection));
  }));

  renderSplitBar(clusterId);
}

// "move them to a new or existing cluster/person" (028 decision #4) — two
// buttons rather than one prompt that treats an empty name as "new group",
// since the shared `promptTagNameInput` helper already treats an empty
// commit as cancel everywhere else; reusing it here with a different
// meaning for "empty" would be a silent, easy-to-miss exception to that
// rule.
function renderSplitBar(clusterId) {
  const bar = document.getElementById(`split-${clusterId}`);
  const select = getSplitSelect(clusterId);
  if (select.size === 0) { bar.style.display = 'none'; bar.innerHTML = ''; return; }

  bar.style.display = 'flex';
  bar.innerHTML = `
    <span>${select.size} face${select.size === 1 ? '' : 's'} selected</span>
    <button class="btn" data-move-new="${clusterId}">Move to new group</button>
    <button class="btn" data-move-named="${clusterId}">Move to named person…</button>
  `;

  const moveSelected = async (personName) => {
    try {
      await invoke('move_face_detections_to_new_cluster', { detectionIds: [...select.selection], personName });
      showToast(personName ? `Moved to ${personName}` : 'Moved to a new group');
      // No explicit splitSelections.delete(clusterId) here — renderPeopleView()
      // below unconditionally clears the whole Map itself (every crop panel
      // collapses on its full re-render anyway).
      renderPeopleView();
    } catch (err) { showToast('Could not move these faces'); }
  };

  bar.querySelector('[data-move-new]').addEventListener('click', () => moveSelected(null));
  bar.querySelector('[data-move-named]').addEventListener('click', (e) => {
    promptTagNameInput(e.currentTarget, (value) => moveSelected(value), { placeholder: 'person’s name…', listId: 'personNames' });
  });
}

// ── Cluster merge (028 decision #4, Milestone ML-4 Slice D2) ───────────────
// Pairwise only — the spec's "side-by-side" comparison card has no shape
// for 3+ candidates — so selection caps at 2, unlike the grid's bulk-select
// (Slice B), which allows any number.
let mergeSelection = new Set();
let peopleClustersCache = [];
let mergeKeeperId = null;

// Shared by `toggleMergeSelection`/`clearMergeSelection` — both need to
// keep a card's outline and its checkbox glyph in sync with `mergeSelection`.
function syncPickUI(card, picked) {
  card.classList.toggle('people-card-picked', picked);
  const pick = card.querySelector('[data-pick-cluster]');
  if (pick) pick.innerHTML = picked ? checkIcon() : '';
}

function toggleMergeSelection(clusterId) {
  if (mergeSelection.has(clusterId)) {
    mergeSelection.delete(clusterId);
  } else {
    if (mergeSelection.size >= 2) { showToast('Merge compares two groups at a time — deselect one first'); return; }
    mergeSelection.add(clusterId);
  }
  document.querySelectorAll('.people-card').forEach(card => {
    syncPickUI(card, mergeSelection.has(Number(card.dataset.clusterCard)));
  });
  updateMergeBar();
}

function updateMergeBar() {
  const bar = document.getElementById('mergeBar');
  bar.style.display = mergeSelection.size === 2 ? 'flex' : 'none';
}

function clearMergeSelection() {
  mergeSelection.clear();
  document.querySelectorAll('.people-card.people-card-picked').forEach(card => syncPickUI(card, false));
  updateMergeBar();
}
document.getElementById('mergeClearBtn').addEventListener('click', clearMergeSelection);

// Renders one side of the side-by-side comparison, reusing dedupe's own
// .review-candidate/.suggested-badge shape (028 decision #4: "reuses
// dedupe's already-locked review-card shape directly"). Clicking a
// candidate makes it the keeper — "a pre-selected suggestion, human can
// override."
function renderMergeCandidate(cluster, isKeeper) {
  return `
    <div class="review-candidate${isKeeper ? ' suggested' : ''}" data-merge-candidate="${cluster.id}" style="cursor:pointer">
      ${isKeeper ? `<div class="suggested-badge">KEEPER</div>` : ''}
      <div class="review-candidate-img">${cluster.representativeCropPath ? `<img src="${assetSrc(cluster.representativeCropPath)}" alt="">` : ''}</div>
      <div class="candidate-meta">${cluster.personName ? escapeHtml(cluster.personName) : 'Unnamed'}<br>${cluster.photoCount} photo${cluster.photoCount === 1 ? '' : 's'}</div>
    </div>`;
}

function renderMergeNameConflict(a, b) {
  const el = document.getElementById('mergeNameConflict');
  if (!a.personName || !b.personName || a.personName.toLowerCase() === b.personName.toLowerCase()) {
    el.style.display = 'none';
    return;
  }
  // "present both, human picks one (or types a third) — never silently choose."
  el.style.display = 'block';
  el.innerHTML = `
    <div class="merge-name-label">These groups are named differently — which name should the merged group use?</div>
    <span class="merge-name-option" data-name-choice="${escapeHtml(a.personName)}">${escapeHtml(a.personName)}</span>
    <span class="merge-name-option" data-name-choice="${escapeHtml(b.personName)}">${escapeHtml(b.personName)}</span>
    <input class="tag-input" id="mergeNameCustom" placeholder="or type a different name…" list="personNames">
  `;
  el.querySelectorAll('[data-name-choice]').forEach(opt => opt.addEventListener('click', () => {
    el.querySelectorAll('.merge-name-option').forEach(o => o.classList.remove('checked'));
    opt.classList.add('checked');
    document.getElementById('mergeNameCustom').value = '';
  }));
}

// Resolves the 2 selected cluster ids in `mergeSelection` back to their
// full cluster objects — shared by `renderMergeModal` (opening the
// confirmation) and the confirm handler (acting on it), both of which
// need the same pair looked up the same way.
function getMergeCandidates() {
  const [idA, idB] = [...mergeSelection];
  return [peopleClustersCache.find(c => c.id === idA), peopleClustersCache.find(c => c.id === idB)];
}

function renderMergeModal() {
  const [clusterA, clusterB] = getMergeCandidates();
  if (!clusterA || !clusterB) { showToast('Could not open merge — try reselecting'); return; }

  // Pre-selected suggestion: the cluster with more photos (real/established
  // groups outrank thin ones, same reasoning as the People view's own
  // photo-count-descending sort).
  mergeKeeperId = clusterA.photoCount >= clusterB.photoCount ? clusterA.id : clusterB.id;
  renderMergeCandidates(clusterA, clusterB);
  // Rendered once, not on every keeper swap below: which name the merged
  // group ends up with is independent of which cluster row survives as
  // "keeper" — swapping keeper must not discard an in-progress name pick.
  renderMergeNameConflict(clusterA, clusterB);
  document.getElementById('mergeModal').classList.add('open');
}

function renderMergeCandidates(clusterA, clusterB) {
  const compare = document.getElementById('mergeCompare');
  const [keeper, other] = mergeKeeperId === clusterA.id ? [clusterA, clusterB] : [clusterB, clusterA];
  compare.innerHTML = renderMergeCandidate(keeper, true) + renderMergeCandidate(other, false);
  compare.querySelectorAll('[data-merge-candidate]').forEach(card => card.addEventListener('click', () => {
    mergeKeeperId = Number(card.dataset.mergeCandidate);
    renderMergeCandidates(clusterA, clusterB);
  }));
}

function closeMergeModal() {
  document.getElementById('mergeModal').classList.remove('open');
}
document.getElementById('mergeCancelBtn').addEventListener('click', closeMergeModal);

document.getElementById('mergeSelectedBtn').addEventListener('click', () => {
  if (mergeSelection.size !== 2) return;
  renderMergeModal();
});

document.getElementById('mergeConfirmBtn').addEventListener('click', async () => {
  const [clusterA, clusterB] = getMergeCandidates();
  if (!clusterA || !clusterB) return;
  const loserId = mergeKeeperId === clusterA.id ? clusterB.id : clusterA.id;

  // Resulting name: the checked conflict option/typed text when a real
  // conflict was shown; otherwise whichever side (if either) already has
  // one — there's nothing to ask about when only one side is named or
  // both already agree.
  let resultingPersonName = null;
  const conflictEl = document.getElementById('mergeNameConflict');
  if (conflictEl.style.display !== 'none') {
    const custom = document.getElementById('mergeNameCustom').value.trim();
    const checked = conflictEl.querySelector('.merge-name-option.checked');
    resultingPersonName = custom || checked?.dataset.nameChoice || null;
    if (!resultingPersonName) { showToast('Pick a name or type one first'); return; }
  } else {
    resultingPersonName = clusterA.personName || clusterB.personName || null;
  }

  try {
    await invoke('merge_face_clusters', { keeperClusterId: mergeKeeperId, loserClusterId: loserId, resultingPersonName });
    showToast('Merged');
    closeMergeModal();
    clearMergeSelection();
    renderPeopleView();
  } catch (err) { showToast('Could not merge these groups'); }
});

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
  document.getElementById('view-albums').style.display = currentView === 'albums' ? 'flex' : 'none';
  if (currentView === 'grid') layout();
  if (currentView === 'review') renderReviewQueue();
  if (currentView === 'people') renderPeopleView();
  if (currentView === 'albums') renderAlbumsView();
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
document.getElementById('view-albums').style.display = 'none';

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

// ── Locking setup (workplan/LOCK-SPEC.md §6) ─────────────────────────────
// Opt-in per vault, decided here at first-run creation (ticket 043) — never
// available for "open existing vault," since encryption is fixed at
// creation, and never silently defaulted on.
let lockingSetup = null; // { keypairPath, password } | null, set once fully configured
let lockingSupported = false;

async function refreshLockingAvailability() {
  try {
    lockingSupported = await invoke('check_windows_edition_supports_locking');
  } catch (e) {
    lockingSupported = false;
  }
  const toggle = document.getElementById('lockingToggle');
  const blocked = document.getElementById('lockingEditionBlocked');
  if (lockingSupported) {
    toggle.classList.remove('disabled');
    blocked.style.display = 'none';
  } else {
    toggle.classList.add('disabled');
    toggle.classList.remove('on');
    blocked.style.display = 'block';
    lockingSetup = null;
  }
}

// 'firstrun' (default): confirming just records lockingSetup for
// firstrunConfirmBtn to consume when it creates the vault.
// 'migrate': confirming calls migrate_vault_to_encrypted directly against
// the already-open vault (ticket 050's "enable Locking on an existing
// library" path) — there's no separate "create" step to chain into.
let lockingSetupMode = 'firstrun';

function openLockingSetupModal(mode) {
  lockingSetupMode = mode || 'firstrun';
  document.getElementById('lockingKeyStatus').textContent = '';
  document.getElementById('lockingPasswordInput').value = '';
  document.getElementById('lockingPasswordConfirmInput').value = '';
  document.getElementById('lockingConfirmPhraseInput').value = '';
  document.getElementById('lockingSetupConfirmBtn').disabled = true;
  document.getElementById('lockingSetupConfirmBtn').textContent = 'Enable Encryption';
  lockingModalKeypairPath = null;
  document.getElementById('lockingSetupModal').classList.add('open');
}
function closeLockingSetupModal() {
  document.getElementById('lockingSetupModal').classList.remove('open');
}

let lockingModalKeypairPath = null;

document.getElementById('lockingToggle').addEventListener('click', () => {
  const toggle = document.getElementById('lockingToggle');
  if (toggle.classList.contains('disabled')) return;
  if (toggle.classList.contains('on')) {
    // Turning back off is friction-free — nothing destructive has
    // happened yet at this point, the vault doesn't exist until
    // firstrunConfirmBtn is clicked.
    toggle.classList.remove('on');
    lockingSetup = null;
    return;
  }
  openLockingSetupModal();
});

document.getElementById('lockingChooseKeyFolderBtn').addEventListener('click', async () => {
  let folder;
  try { folder = await invoke('pick_library_folder'); } catch (e) { return; }
  if (!folder) return;
  const btn = document.getElementById('lockingChooseKeyFolderBtn');
  const status = document.getElementById('lockingKeyStatus');
  btn.disabled = true;
  status.textContent = 'Generating…';
  try {
    lockingModalKeypairPath = await invoke('generate_vault_keypair', { destDir: folder });
    status.textContent = `Key file: ${lockingModalKeypairPath}`;
  } catch (e) {
    status.textContent = '';
    showToast('Could not generate a key file there');
    lockingModalKeypairPath = null;
  }
  btn.disabled = false;
  updateLockingConfirmEnabled();
});

function updateLockingConfirmEnabled() {
  const password = document.getElementById('lockingPasswordInput').value;
  const confirm = document.getElementById('lockingPasswordConfirmInput').value;
  const phrase = document.getElementById('lockingConfirmPhraseInput').value.trim().toLowerCase();
  const ok = !!lockingModalKeypairPath
    && password.length >= 12
    && password === confirm
    && phrase === 'i understand this cannot be recovered';
  document.getElementById('lockingSetupConfirmBtn').disabled = !ok;
}
['lockingPasswordInput', 'lockingPasswordConfirmInput', 'lockingConfirmPhraseInput'].forEach(id => {
  document.getElementById(id).addEventListener('input', updateLockingConfirmEnabled);
});

document.getElementById('lockingSetupCancelBtn').addEventListener('click', () => {
  document.getElementById('lockingToggle').classList.remove('on');
  lockingSetup = null;
  closeLockingSetupModal();
});

document.getElementById('lockingSetupConfirmBtn').addEventListener('click', async () => {
  const password = document.getElementById('lockingPasswordInput').value;
  const keypairPath = lockingModalKeypairPath;

  if (lockingSetupMode === 'migrate') {
    const btn = document.getElementById('lockingSetupConfirmBtn');
    btn.disabled = true;
    btn.textContent = 'Migrating… approve the Windows permission prompt if asked';
    try {
      await invoke('migrate_vault_to_encrypted', { keypairPath, password, sizeBytes: null });
    } catch (e) {
      showToast('Could not encrypt this vault — it stays as it was, nothing was changed.');
      btn.disabled = false;
      btn.textContent = 'Enable Encryption';
      return;
    }
    closeLockingSetupModal();
    showToast('Vault encrypted.');
    document.getElementById('settingsEnableLockingRow').style.display = 'none';
    return;
  }

  lockingSetup = { keypairPath, password };
  document.getElementById('lockingToggle').classList.add('on');
  closeLockingSetupModal();
});

document.getElementById('settingsEnableLockingBtn').addEventListener('click', () => {
  closeSettingsModal();
  openLockingSetupModal('migrate');
});

document.getElementById('firstrunConfirmBtn').addEventListener('click', async () => {
  if (!firstrunChoice) return;
  const btn = document.getElementById('firstrunConfirmBtn');
  btn.disabled = true;
  const verb = firstrunChoice.existingLibrary ? 'Opening' : 'Creating';
  btn.textContent = lockingSetup ? `${verb}… approve the Windows permission prompt if asked` : `${verb}…`;
  try {
    if (firstrunChoice.existingLibrary) {
      await invoke('open_existing_library', { path: firstrunChoice.path });
    } else {
      const conversionEnabled = document.getElementById('conversionToggle').classList.contains('on');
      if (lockingSetup) {
        await invoke('create_encrypted_vault', {
          root: firstrunChoice.path,
          keypairPath: lockingSetup.keypairPath,
          password: lockingSetup.password,
          conversionEnabled,
          sizeBytes: null,
        });
      } else {
        await invoke('create_library', { path: firstrunChoice.path, conversionEnabled });
      }
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
  lockingSetup = null;
  startMainApp();
});

refreshLockingAvailability();

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
  // Locking's own edition support was already checked once at boot
  // (refreshLockingAvailability) — no need to re-check on every open.
  // Whether *this* vault is already encrypted isn't currently exposed by
  // any status DTO, so the row shows whenever the edition supports it;
  // migrate_vault_to_encrypted itself refuses cleanly if already
  // encrypted (surfaced as a toast, same as any other backend error).
  document.getElementById('settingsEnableLockingRow').style.display = lockingSupported ? 'flex' : 'none';
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

// ── About (ML-SPEC.md §10, ticket 032 decision #3, Milestone ML-6) ─────────
// Static content — model licenses are hardcoded here (they only change on
// a deliberate model swap, same cadence as MODELS.md itself), not fetched
// from the backend. The full third-party dependency list lives in
// LICENSES.txt (too large for a modal — 500+ crates) rather than inline;
// "Show licenses file" uses revealItemInDir (already covered by the
// opener plugin's default permission grant) instead of openPath (would
// need a new, unscoped "open any path" permission grant for one button).
const { revealItemInDir } = window.__TAURI__.opener;

function openAboutModal() {
  document.getElementById('aboutModal').classList.add('open');
  invoke('get_app_version').then(v => {
    document.getElementById('aboutVersion').textContent = v;
  }).catch(() => {});
}
function closeAboutModal() { document.getElementById('aboutModal').classList.remove('open'); }
document.getElementById('railAboutBtn').addEventListener('click', openAboutModal);
document.getElementById('aboutCloseBtn').addEventListener('click', closeAboutModal);
document.getElementById('aboutShowLicensesBtn').addEventListener('click', async () => {
  try {
    const path = await invoke('get_licenses_file_path');
    await revealItemInDir(path);
  } catch (e) {
    showToast('Could not show the licenses file');
  }
});

// ── Boot ───────────────────────────────────────────────────────────────
// `check_library_status` decides between the first-run screen (true first
// run, or a previously-configured library whose path is no longer
// reachable — an unplugged external drive, say) and the main app. Only
// once a live library exists do we call anything that touches the
// catalog — list_images/list_review_queue/etc. would otherwise error.
renderSortPop();

// ── Background analysis ambient badge (ML-SPEC.md §9, ticket 030,
// Milestone ML-6) ───────────────────────────────────────────────────────
// Never a modal — "background maintenance the user didn't explicitly
// start" (§9) — a rail badge + a small popover with pause/resume, same
// shape as the People/review-queue badges but with two update sources:
// pulled once at boot (`get_analysis_status`, same reason the other
// badges have their own boot-time refresh — the push-only event would
// otherwise leave it blank for up to the loop's idle-sleep interval) and
// pushed live via the `analysis-progress` event the whole time the app
// runs, regardless of which view is open.
let analysisPaused = false;

// A pending entry means ticket 030 decision #4's gate is closed for that
// model — the backend already excludes it from real processing, so this
// is purely "ask, then let refreshAnalysisBadge() pull the real new
// counts" rather than predicting them client-side.
function renderAnalysisPop(taggingRemaining, facesRemaining, pendingUpgrades) {
  const pop = document.getElementById('analysisPop');
  const total = taggingRemaining + facesRemaining;
  const upgrades = pendingUpgrades || [];
  const upgradesHtml = upgrades.map(u => `
    <div style="padding:6px 8px; font-size:11.5px; line-height:1.5">
      A newer ${escapeHtml(u.modelName)} model is available (${escapeHtml(u.oldVersion)} → ${escapeHtml(u.newVersion)}). Re-analyzing will process ${u.backlogCount.toLocaleString()} image${u.backlogCount === 1 ? '' : 's'}.
      <button class="popover-link run-upgrade-btn" data-notice-id="${u.id}" style="display:block; width:100%; text-align:left; padding:4px 0">Run now</button>
    </div>
  `).join('');
  pop.innerHTML = `
    ${upgradesHtml}${upgradesHtml ? '<div class="popover-divider"></div>' : ''}
    <div style="padding:6px 8px; font-size:11.5px; color:var(--text-muted); line-height:1.6">
      ${total === 0 ? 'Everything is analyzed.' : `Tagging: ${taggingRemaining} left<br>Faces: ${facesRemaining} left`}
    </div>
    <div class="popover-divider"></div>
    <button class="popover-link" id="analysisPauseBtn" style="width:100%; text-align:left; padding:6px 8px">${analysisPaused ? 'Resume' : 'Pause'} analysis</button>
  `;
  document.getElementById('analysisPauseBtn').addEventListener('click', async (e) => {
    e.stopPropagation();
    analysisPaused = !analysisPaused;
    try { await invoke('set_analysis_paused', { paused: analysisPaused }); } catch (err) { console.error(err); }
    renderAnalysisPop(taggingRemaining, facesRemaining, pendingUpgrades);
  });
  pop.querySelectorAll('.run-upgrade-btn').forEach((btn) => {
    btn.addEventListener('click', async (e) => {
      e.stopPropagation();
      const noticeId = Number(btn.dataset.noticeId);
      try {
        await invoke('accept_model_upgrade', { noticeId });
        await refreshAnalysisBadge();
      } catch (err) {
        console.error(err);
        showToast('Could not start re-analysis.');
      }
    });
  });
}

function updateAnalysisBadge(taggingRemaining, facesRemaining, pendingUpgrades) {
  const badge = document.getElementById('analysisBadge');
  const total = taggingRemaining + facesRemaining;
  if (total > 0) { badge.style.display = 'flex'; badge.textContent = total > 999 ? '999+' : String(total); }
  else badge.style.display = 'none';
  renderAnalysisPop(taggingRemaining, facesRemaining, pendingUpgrades);
}

async function refreshAnalysisBadge() {
  let status;
  try { status = await invoke('get_analysis_status'); } catch (e) { return; }
  analysisPaused = status.paused;
  updateAnalysisBadge(status.taggingRemaining, status.facesRemaining, status.pendingUpgrades);
}

document.getElementById('analysisBtn').addEventListener('click', (e) => togglePop('analysisPop', e));

// Fire-and-forget: this listener lives for the whole app session, so
// there's no matching unlisten call the way import's own progress
// listener has (that one is scoped to a single import run).
listen('analysis-progress', (event) => {
  updateAnalysisBadge(event.payload.taggingRemaining, event.payload.facesRemaining, event.payload.pendingUpgrades);
});

function startMainApp() {
  refresh();
  refreshReviewBadge();
  refreshPeopleBadge();
  refreshAnalysisBadge();
  // Populated here too, not only on first People-view visit — the
  // drawer's inline-naming chip (`renderDrawerPeople`) needs the
  // autocomplete datalist to already exist even if the user never opens
  // the People view first.
  invoke('list_persons').then(renderPersonDatalist).catch(() => {});
}

// ── Unlock screen (workplan/LOCK-SPEC.md §5, ticket 047) ─────────────────
function showUnlockScreen(locked) {
  document.getElementById('unlockScreen').classList.remove('hidden');
  document.getElementById('mainApp').classList.add('hidden');
  document.getElementById('firstrun').classList.add('hidden');
  document.getElementById('unlockVaultRoot').textContent = locked.vaultRoot;
  document.getElementById('unlockKeypairHint').textContent = locked.keypairPathHint;
  document.getElementById('unlockErrorBanner').style.display = 'none';
  document.getElementById('unlockPasswordInput').value = '';
  currentLockedVaultRoot = locked.vaultRoot;
}

let currentLockedVaultRoot = null;

// CmdError has no structured error code anywhere in this codebase (it
// always serializes to a plain string, ticket-independent — see
// `impl Serialize for CmdError`) — matching that existing convention
// rather than introducing a stronger-typed channel just for these two
// messages. Exact text must stay in sync with src-tauri/src/lib.rs's
// KeypairFileMissing/IncorrectPasswordOrKey Display strings.
function unlockErrorMessage(rawError) {
  const text = String(rawError);
  if (text.includes("key file could not be found")) {
    return "The key file wasn't found at the path shown above. Check it's connected (e.g. a USB key) and try again.";
  }
  if (text.includes('incorrect password or key')) {
    return 'Incorrect password or key.';
  }
  return "Couldn't unlock the vault.";
}

document.getElementById('unlockConfirmBtn').addEventListener('click', async () => {
  const password = document.getElementById('unlockPasswordInput').value;
  const btn = document.getElementById('unlockConfirmBtn');
  const banner = document.getElementById('unlockErrorBanner');
  banner.style.display = 'none';
  btn.disabled = true;
  btn.textContent = 'Unlocking… approve the Windows permission prompt if asked';
  try {
    await invoke('unlock_vault', { root: currentLockedVaultRoot, password });
  } catch (e) {
    document.getElementById('unlockErrorText').textContent = unlockErrorMessage(e);
    banner.style.display = 'flex';
    btn.disabled = false;
    btn.textContent = 'Unlock';
    return;
  }
  btn.disabled = false;
  btn.textContent = 'Unlock';
  showMainApp();
  startMainApp();
});

document.getElementById('unlockPasswordInput').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') document.getElementById('unlockConfirmBtn').click();
});

// Surfaces `lock_vault_now`'s failure on app-close (ticket 046's "still in
// use" case) — the window didn't actually close, so the user needs to
// know why rather than just seeing nothing happen.
listen('vault-lock-failed', (event) => {
  showToast(String(event.payload));
});

async function boot() {
  let status;
  try {
    status = await invoke('check_library_status');
  } catch (e) {
    console.error(e);
    status = { ready: false, previousPathUnreachable: null, locked: null };
  }
  if (status.ready) {
    showMainApp();
    startMainApp();
  } else if (status.locked) {
    showUnlockScreen(status.locked);
  } else {
    showFirstRun(status.previousPathUnreachable);
  }
}
boot();
