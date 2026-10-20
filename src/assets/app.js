'use strict';

const $ = (selector, root = document) => root.querySelector(selector);
const $$ = (selector, root = document) => Array.from(root.querySelectorAll(selector));

const TOKEN = document.querySelector('meta[name="tmv-token"]').content;

const state = {
  dirs: [],
  grid: { rows: 2, cols: 2 },
  shuffle: true,
  loop: false,
  mute: true,
  ffmpegAvailable: false,
};

const counts = new Map();
const CODEC_SUSPECT = ['mkv', 'avi', 'flv', 'wmv', 'hevc', 'h265'];

let wall = null;
let cells = [];
let paused = false;
let muted = true;
let hudTimer = null;
let toastTimer = null;
let fsCell = null;
let metaText = '';

// ------------------------------------------------------------------ helpers

function basename(path) {
  const parts = String(path).split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function stem(path) {
  const name = basename(path);
  const dot = name.lastIndexOf('.');
  return dot > 0 ? name.slice(0, dot) : name;
}

function extOf(path) {
  const name = basename(path);
  const dot = name.lastIndexOf('.');
  return dot >= 0 ? name.slice(dot + 1).toLowerCase() : '';
}

function fmtTime(seconds) {
  if (!Number.isFinite(seconds) || seconds < 0) return '0:00';
  const total = Math.floor(seconds);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, '0')}`;
}

function humanSize(bytes) {
  const value = Number(bytes) || 0;
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  if (value >= GB) return `${(value / GB).toFixed(1)} GB`;
  if (value >= MB) return `${(value / MB).toFixed(1)} MB`;
  if (value >= KB) return `${(value / KB).toFixed(1)} KB`;
  return `${value} B`;
}

function withSeek(url, seconds) {
  if (!url) return url;
  return `${url}${url.includes('?') ? '&' : '?'}t=${seconds}`;
}

async function api(path, body) {
  const init = body === undefined
    ? { headers: { 'x-tmv-token': TOKEN } }
    : {
        method: 'POST',
        headers: { 'content-type': 'application/json', 'x-tmv-token': TOKEN },
        body: JSON.stringify(body),
      };
  const response = await fetch(path, init);
  return response.json();
}

function toast(message) {
  const el = $('#toast');
  el.textContent = message;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { el.hidden = true; }, 2500);
}

function showError(message) {
  $('#errText').textContent = message;
  $('#errModal').hidden = false;
}

// -------------------------------------------------------------------- lobby

function setLobbyError(message) {
  $('#lobbyStatus').textContent = message || '';
}

function renderDirs() {
  const list = $('#dirList');
  list.textContent = '';
  $('#dirEmpty').hidden = state.dirs.length > 0;

  state.dirs.forEach((dir, index) => {
    const row = document.createElement('div');
    row.className = 'dir-row';

    const path = document.createElement('span');
    path.className = 'dir-path';
    path.textContent = dir;
    path.title = dir;

    const count = document.createElement('span');
    count.className = 'dir-count';
    const label = counts.get(dir);
    count.textContent = label === undefined ? '…' : label;
    if (label === 'not found') count.classList.add('error');

    const remove = document.createElement('button');
    remove.className = 'dir-remove';
    remove.textContent = '×';
    remove.title = 'Remove folder';
    remove.addEventListener('click', () => {
      state.dirs.splice(index, 1);
      renderDirs();
    });

    row.append(path, count, remove);
    list.append(row);
  });
}

async function refreshCounts(dirs) {
  const unique = [...new Set(dirs)];
  if (!unique.length) return;
  unique.forEach((dir) => counts.set(dir, '…'));
  renderDirs();
  try {
    const result = await api('/api/scan', { dirs: unique });
    (result.counts || []).forEach((entry) => {
      if (entry.error) counts.set(entry.dir, 'not found');
      else counts.set(entry.dir, entry.count === 1 ? '1 video' : `${entry.count} videos`);
    });
  } catch {
    unique.forEach((dir) => counts.set(dir, 'not found'));
  }
  renderDirs();
}

function clampInt(value, fallback) {
  const parsed = parseInt(value, 10);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.min(8, Math.max(1, parsed));
}

function syncGrid() {
  state.grid = {
    rows: clampInt($('#rows').value, 2),
    cols: clampInt($('#cols').value, 2),
  };
  $('#rows').value = state.grid.rows;
  $('#cols').value = state.grid.cols;
  $('#cellLabel').textContent = `${state.grid.rows * state.grid.cols} videos`;
  $$('.preset').forEach((button) => {
    button.classList.toggle(
      'active',
      Number(button.dataset.rows) === state.grid.rows && Number(button.dataset.cols) === state.grid.cols,
    );
  });
}

async function addFolders() {
  let picked;
  try {
    picked = await api('/api/pick-directories', {});
  } catch (err) {
    setLobbyError(`Failed to scan folders: ${err.message}`);
    return;
  }
  const added = [];
  (picked.dirs || []).forEach((dir) => {
    if (!state.dirs.includes(dir)) {
      state.dirs.push(dir);
      added.push(dir);
    }
  });
  renderDirs();
  refreshCounts(added);
}

async function startWall(auto) {
  setLobbyError('');
  if (!state.dirs.length) {
    setLobbyError('Add at least one folder');
    return;
  }
  const button = $('#startBtn');
  button.disabled = true;
  button.textContent = 'Starting...';

  const payload = {
    dirs: state.dirs,
    grid: state.grid,
    shuffle: $('#shuffle').checked,
    loop: $('#loop').checked,
    mute: $('#mute').checked,
  };

  let result;
  try {
    result = await api('/api/start-wall', payload);
  } catch (err) {
    result = { ok: false, error: `Failed to start: ${err.message}` };
  }

  button.disabled = false;
  button.textContent = 'Start Wall';

  if (!result.ok) {
    const message = result.error || 'Failed to start';
    if (auto) showError(message);
    else setLobbyError(message);
    return;
  }

  state.shuffle = payload.shuffle;
  state.loop = payload.loop;
  state.mute = payload.mute;
  enterWall(result.wall);
}

// --------------------------------------------------------------------- wall

function enterWall(config) {
  wall = config;
  paused = false;
  muted = !!config.mute;
  $('#lobby').hidden = true;
  $('#wall').hidden = false;
  buildGrid(config.grid);
  $('#pause').textContent = 'Pause';
  $('#mute').textContent = muted ? 'Unmute' : 'Mute';
  updateStatus();
  hudWake();

  config.videos.forEach((entry, index) => {
    setTimeout(() => {
      if (!wall) return;
      loadEntry(cells[index], entry);
    }, index * 250);
  });
}

function buildGrid(grid) {
  cells.forEach((cell) => {
    cell.video.pause();
    cell.video.removeAttribute('src');
    cell.video.load();
  });
  cells = [];

  const container = $('#grid');
  container.textContent = '';
  container.style.gridTemplateColumns = `repeat(${grid.cols}, 1fr)`;
  container.style.gridTemplateRows = `repeat(${grid.rows}, 1fr)`;

  const total = grid.rows * grid.cols;
  for (let index = 0; index < total; index += 1) {
    const el = document.createElement('div');
    el.className = 'cell';

    const video = document.createElement('video');
    video.playsInline = true;
    video.preload = 'auto';
    video.crossOrigin = 'anonymous';
    video.muted = muted;

    const loading = document.createElement('div');
    loading.className = 'loading';
    loading.textContent = 'Loading...';
    loading.hidden = true;

    const message = document.createElement('div');
    message.className = 'cellmsg';
    message.hidden = true;

    const overlay = buildOverlay(index);

    el.append(video, loading, message, overlay);
    container.append(el);

    const cell = {
      index,
      el,
      video,
      loading,
      message,
      overlay,
      entry: null,
      clickTimer: 0,
      rot: 0,
      usingFallback: false,
      blackTries: 0,
      blackTimer: 0,
      loadTimer: 0,
      firstCheckDone: false,
    };
    cells.push(cell);

    message.addEventListener('click', (event) => {
      event.stopPropagation();
      if (!cell.entry) return;
      cell.video.play().then(() => setCellMessage(cell, '')).catch(() => {});
    });

    el.addEventListener('click', (event) => {
      if (event.target.closest('.overlay')) return;
      if (!cell.entry) return;
      clearTimeout(cell.clickTimer);
      cell.clickTimer = setTimeout(() => toggleCellPlay(cell), 250);
    });

    el.addEventListener('dblclick', (event) => {
      if (event.target.closest('.overlay')) return;
      clearTimeout(cell.clickTimer);
      if (cell.entry) enterFullscreen(cell);
    });
  }
}

function button(label, handler) {
  const element = document.createElement('button');
  element.textContent = label;
  element.addEventListener('click', (event) => {
    event.stopPropagation();
    handler();
  });
  return element;
}

function buildOverlay(index) {
  const overlay = document.createElement('div');
  overlay.className = 'overlay';

  const pick = document.createElement('div');
  pick.className = 'pick-list';
  pick.hidden = true;

  const seekRow = document.createElement('div');
  seekRow.className = 'seek-row';
  const slider = document.createElement('input');
  slider.type = 'range';
  slider.min = '0';
  slider.max = '100';
  slider.step = '0.1';
  slider.value = '0';
  const time = document.createElement('span');
  time.textContent = '0:00 / 0:00';
  slider.addEventListener('input', () => {
    const cell = cells[index];
    if (!cell || !cell.entry) return;
    const duration = Number.isFinite(cell.video.duration) ? cell.video.duration : 0;
    time.textContent = `${fmtTime((Number(slider.value) / 100) * duration)} / ${fmtTime(duration)}`;
  });
  slider.addEventListener('pointerup', () => {
    const cell = cells[index];
    if (cell) doSeek(cell, Number(slider.value) / 100);
  });
  seekRow.append(slider, time);

  const row = document.createElement('div');
  row.className = 'btn-row';
  row.append(
    button('⏩ Next', () => nextVideo(cells[index])),
    button('☰ Pick', () => togglePick(cells[index])),
    button('Meta', () => openMeta(cells[index])),
    button('Del', () => deleteVideo(cells[index])),
  );
  const rotateGroup = document.createElement('div');
  rotateGroup.className = 'rot-group';
  rotateGroup.append(
    button('↶', () => rotate(cells[index], -90)),
    button('↷', () => rotate(cells[index], 90)),
  );
  row.append(rotateGroup);

  const caption = document.createElement('div');
  caption.className = 'caption';
  const title = document.createElement('div');
  title.className = 'cell-title';
  const info = document.createElement('div');
  info.className = 'cell-info';
  caption.append(title, info);

  overlay.append(pick, seekRow, row, caption);
  return overlay;
}

function resetCell(cell) {
  clearTimeout(cell.blackTimer);
  cell.blackTimer = 0;
  cell.blackTries = 0;
  cell.firstCheckDone = false;
  cell.usingFallback = false;
  cell.rot = 0;
  cell.video.style.transform = '';
  setCellMessage(cell, '');
  hideLoading(cell);
  cell.overlay.querySelector('.pick-list').hidden = true;
}

function clearCell(cell) {
  cell.entry = null;
  const video = cell.video;
  video.pause();
  video.removeAttribute('src');
  video.load();
  cell.overlay.querySelector('.cell-title').textContent = '';
  cell.overlay.querySelector('.cell-info').textContent = '';
  cell.overlay.querySelector('.seek-row span').textContent = '0:00 / 0:00';
  cell.overlay.querySelector('.seek-row input').value = 0;
  updateStatus();
}

function setCellMessage(cell, text) {
  cell.message.textContent = text || '';
  cell.message.hidden = !text;
}

function setLoading(cell, visible) {
  cell.loading.hidden = !visible;
}

function showLoading(cell) {
  clearTimeout(cell.loadTimer);
  cell.loadTimer = 0;
  setLoading(cell, true);
}

function hideLoading(cell) {
  clearTimeout(cell.loadTimer);
  cell.loadTimer = 0;
  setLoading(cell, false);
}

// Brief stalls (loop restarts, seek) must not flash the indicator.
function stallLoading(cell) {
  if (!cell.loading.hidden) return;
  clearTimeout(cell.loadTimer);
  cell.loadTimer = setTimeout(() => {
    if (cell.entry) setLoading(cell, true);
  }, 250);
}

function updateCaption(cell) {
  if (!cell.entry) return;
  cell.overlay.querySelector('.cell-title').textContent = stem(cell.entry.path);
  cell.overlay.querySelector('.cell-info').textContent = humanSize(cell.entry.size);
}

function loadEntry(cell, entry) {
  resetCell(cell);
  if (!entry) {
    clearCell(cell);
    return;
  }
  cell.entry = entry;
  const video = cell.video;
  video.muted = muted;
  video.loop = !!(wall && wall.loop);
  video.src = entry.url;
  video.load();
  showLoading(cell);
  wireVideo(cell, entry);
  updateCaption(cell);
  const attempt = video.play();
  if (attempt && attempt.catch) {
    attempt.catch(() => {
      if (cell.entry === entry) setCellMessage(cell, `Click to play: ${stem(entry.path)}`);
    });
  }
  updateStatus();
}

function wireVideo(cell, entry) {
  const video = cell.video;
  const current = () => cell.entry === entry;

  video.onloadeddata = () => {
    if (!current()) return;
    hideLoading(cell);
    updateStatus();
  };
  video.oncanplay = () => {
    if (!current()) return;
    hideLoading(cell);
  };
  video.onplaying = () => {
    if (!current()) return;
    hideLoading(cell);
    updateStatus();
    scheduleBlackCheck(cell, 1500);
  };
  video.onwaiting = () => {
    if (current()) stallLoading(cell);
  };
  video.onstalled = () => {
    if (current()) stallLoading(cell);
  };
  video.ontimeupdate = () => {
    if (!current()) return;
    if (!cell.firstCheckDone && video.currentTime > 0.5) {
      cell.firstCheckDone = true;
      scheduleBlackCheck(cell, 500);
    }
    if (!video.paused && video.readyState >= 2) hideLoading(cell);
    updateSeekUi(cell);
  };
  video.ondurationchange = () => {
    if (current()) updateSeekUi(cell);
  };
  video.onended = () => {
    if (!current()) return;
    if (!(wall && wall.loop)) nextVideo(cell);
  };
  video.onerror = () => {
    if (current()) onVideoError(cell);
  };
  video.onpause = updateStatus;
  video.onplay = updateStatus;
}

function updateSeekUi(cell) {
  if (!cell.entry) return;
  const video = cell.video;
  const duration = Number.isFinite(video.duration) ? video.duration : 0;
  const position = Number.isFinite(video.currentTime) ? video.currentTime : 0;
  const slider = cell.overlay.querySelector('.seek-row input');
  const label = cell.overlay.querySelector('.seek-row span');
  if (document.activeElement !== slider) {
    slider.value = duration > 0 ? String((position / duration) * 100) : '0';
  }
  label.textContent = `${fmtTime(position)} / ${fmtTime(duration)}`;
  if (fsCell === cell) updateFsUi();
}

function onVideoError(cell) {
  hideLoading(cell);
  const entry = cell.entry;
  if (!entry) return;
  if (entry.fallbackUrl && !cell.usingFallback) {
    useFallback(cell);
    return;
  }
  const ext = extOf(entry.path);
  const code = cell.video.error ? cell.video.error.code : 0;
  let message;
  if (CODEC_SUSPECT.includes(ext)) message = `Codec? : .${ext}`;
  else if (code) message = `Error: .${ext} (code ${code})`;
  else message = `Cannot play: .${ext}`;
  setCellMessage(cell, message);
}

function useFallback(cell) {
  const entry = cell.entry;
  if (!entry || !entry.fallbackUrl) return;
  cell.usingFallback = true;
  showLoading(cell);
  const video = cell.video;
  video.muted = muted;
  video.loop = !!(wall && wall.loop);
  video.src = entry.fallbackUrl;
  video.load();
  const attempt = video.play();
  if (attempt && attempt.catch) {
    attempt.catch(() => {
      if (cell.entry === entry) setCellMessage(cell, `Click to play: ${stem(entry.path)}`);
    });
  }
}

function scheduleBlackCheck(cell, delay) {
  if (!cell.entry || !cell.entry.fallbackUrl || cell.usingFallback) return;
  clearTimeout(cell.blackTimer);
  cell.blackTimer = setTimeout(() => checkBlack(cell), delay);
}

function checkBlack(cell) {
  if (!cell.entry || !cell.entry.fallbackUrl || cell.usingFallback) return;
  const video = cell.video;
  if (!video.videoWidth || video.readyState < 2) return;
  let black;
  try {
    const canvas = document.createElement('canvas');
    canvas.width = 4;
    canvas.height = 4;
    const context = canvas.getContext('2d', { willReadFrequently: true });
    context.drawImage(video, 0, 0, 4, 4);
    const data = context.getImageData(0, 0, 4, 4).data;
    black = true;
    for (let i = 0; i < data.length; i += 4) {
      if (data[i] > 5 || data[i + 1] > 5 || data[i + 2] > 5) {
        black = false;
        break;
      }
    }
  } catch {
    return;
  }
  if (!black) {
    cell.blackTries = 0;
    return;
  }
  cell.blackTries += 1;
  if (cell.blackTries >= 4) useFallback(cell);
  else cell.blackTimer = setTimeout(() => checkBlack(cell), 1000);
}

function availableFor(cell) {
  if (!wall) return [];
  const busy = new Set(
    cells
      .filter((other) => other !== cell && other.entry)
      .map((other) => other.entry.path),
  );
  const own = cell.entry ? cell.entry.path : null;
  return wall.pool.filter((entry) => entry.path !== own && !busy.has(entry.path));
}

function nextVideo(cell) {
  if (!cell || !cell.entry) return;
  const available = availableFor(cell);
  if (!available.length) return;
  loadEntry(cell, available[Math.floor(Math.random() * available.length)]);
}

function togglePick(cell) {
  if (!cell || !cell.entry) return;
  const list = cell.overlay.querySelector('.pick-list');
  if (!list.hidden) {
    list.hidden = true;
    return;
  }
  const available = availableFor(cell);
  list.textContent = '';
  available.forEach((entry) => {
    const item = document.createElement('div');
    item.className = 'pick-item';
    item.textContent = stem(entry.path);
    item.title = entry.path;
    item.addEventListener('click', (event) => {
      event.stopPropagation();
      list.hidden = true;
      loadEntry(cell, entry);
    });
    list.append(item);
  });
  list.hidden = false;
}

function doSeek(cell, fraction) {
  if (!cell || !cell.entry) return;
  const video = cell.video;
  const duration = Number.isFinite(video.duration) ? video.duration : 0;
  if (duration <= 0) return;
  const target = Math.max(0, Math.min(duration, fraction * duration));
  const piped = cell.usingFallback || cell.entry.mode !== 'direct';
  if (!piped) {
    try {
      video.currentTime = target;
    } catch {
      /* ignore */
    }
    return;
  }
  const source = cell.usingFallback ? cell.entry.fallbackUrl : cell.entry.url;
  showLoading(cell);
  video.muted = muted;
  video.loop = !!(wall && wall.loop);
  video.src = withSeek(source, Math.floor(target));
  video.load();
  const attempt = video.play();
  if (attempt && attempt.catch) attempt.catch(() => {});
}

function toggleCellPlay(cell) {
  if (!cell || !cell.entry) return;
  const video = cell.video;
  if (video.paused) {
    video.play().then(() => setCellMessage(cell, '')).catch(() => {});
  } else {
    video.pause();
  }
  updateStatus();
}

function rotate(cell, delta) {
  if (!cell || !cell.entry) return;
  cell.rot = ((cell.rot + delta) % 360 + 360) % 360;
  applyRotation(cell);
}

function applyRotation(cell) {
  const video = cell.video;
  video.style.transform = '';
  if (!cell.entry || !video.videoWidth) return;
  const host = fsCell === cell ? $('#fsVideo') : cell.el;
  const cw = host.clientWidth;
  const ch = host.clientHeight;
  if (!cw || !ch) return;

  const deg = cell.rot;
  let scale = 1;
  if (deg % 180 !== 0) {
    const fit = Math.min(cw / video.videoWidth, ch / video.videoHeight);
    const displayedW = video.videoWidth * fit;
    const displayedH = video.videoHeight * fit;
    scale = Math.min(cw / displayedH, ch / displayedW);
  }
  const parts = [];
  if (deg) parts.push(`rotate(${deg}deg)`);
  if (scale !== 1) parts.push(`scale(${scale})`);
  video.style.transform = parts.join(' ');
}

function updateStatus() {
  if (!wall) return;
  const playing = cells.filter((cell) => cell.entry && !cell.video.paused).length;
  $('#status').textContent = `${wall.grid.rows}x${wall.grid.cols} | ${playing} playing`;
}

function togglePause() {
  if (!wall) return;
  paused = !paused;
  cells.forEach((cell) => {
    if (!cell.entry) return;
    if (paused) cell.video.pause();
    else cell.video.play().catch(() => {});
  });
  $('#pause').textContent = paused ? 'Resume' : 'Pause';
  updateStatus();
}

function toggleMute() {
  if (!wall) return;
  muted = !muted;
  cells.forEach((cell) => {
    cell.video.muted = muted;
  });
  $('#mute').textContent = muted ? 'Unmute' : 'Mute';
}

async function reshuffle() {
  if (!wall) return;
  let result;
  try {
    result = await api('/api/reshuffle', {});
  } catch (err) {
    toast(`Reshuffle failed: ${err.message}`);
    return;
  }
  if (result.error) {
    toast(result.error);
    return;
  }
  wall.videos = result.videos || [];
  wall.pool = result.pool || wall.pool;
  wall.loop = !!result.loop;
  wall.mute = !!result.mute;
  paused = false;
  muted = !!result.mute;
  $('#pause').textContent = 'Pause';
  $('#mute').textContent = muted ? 'Unmute' : 'Mute';
  cells.forEach((cell, index) => loadEntry(cell, wall.videos[index] || null));
  updateStatus();
}

async function openMeta(cell) {
  if (!cell || !cell.entry) {
    toast('No video loaded yet');
    return;
  }
  let result;
  try {
    result = await api('/api/meta', { id: cell.entry.id });
  } catch (err) {
    toast(`Meta error: ${err.message}`);
    return;
  }
  if (result.error) {
    toast(`Meta error: ${result.error}`);
    return;
  }
  metaText = result.text || '';
  $('#metaText').textContent = metaText;
  $('#metaModal').hidden = false;
}

function closeMeta() {
  $('#metaModal').hidden = true;
}

async function deleteVideo(cell) {
  if (!cell || !cell.entry) {
    toast('No video loaded yet');
    return;
  }
  const entry = cell.entry;
  let result;
  try {
    result = await api('/api/delete', { id: entry.id });
  } catch (err) {
    toast(`Delete failed: ${err.message}`);
    return;
  }
  if (result.cancelled) return;
  if (result.error) {
    toast(`Delete failed: ${result.error}`);
    return;
  }
  toast(`Deleted ${basename(result.path)}`);
  wall.pool = wall.pool.filter((item) => item.path !== result.path);
  wall.videos = wall.videos.filter((item) => item.path !== result.path);

  const available = availableFor(cell);
  clearCell(cell);
  if (available.length) loadEntry(cell, available[Math.floor(Math.random() * available.length)]);
  else setCellMessage(cell, 'No videos left');
  updateStatus();
}

// --------------------------------------------------------------- fullscreen

function enterFullscreen(cell) {
  if (!cell || !cell.entry) return;
  fsCell = cell;
  cell.video.pause();
  const stage = $('#fsVideo');
  stage.textContent = '';
  stage.append(cell.video);
  $('#fsTitle').textContent = stem(cell.entry.path);
  $('#fsInfo').textContent = humanSize(cell.entry.size);
  $('#fsSlider').value = '0';
  $('#fsTime').textContent = '0:00 / 0:00';
  $('#fs').hidden = false;
  updateFsUi();
  applyRotation(cell);
}

function exitFullscreen() {
  if (!fsCell) return;
  const cell = fsCell;
  fsCell = null;
  cell.el.insertBefore(cell.video, cell.el.firstChild);
  $('#fs').hidden = true;
  applyRotation(cell);
  cell.video.play().catch(() => {});
  updateStatus();
}

function updateFsUi() {
  if (!fsCell) return;
  const video = fsCell.video;
  const duration = Number.isFinite(video.duration) ? video.duration : 0;
  const position = Number.isFinite(video.currentTime) ? video.currentTime : 0;
  const slider = $('#fsSlider');
  if (document.activeElement !== slider) {
    slider.value = duration > 0 ? String((position / duration) * 100) : '0';
  }
  $('#fsTime').textContent = `${fmtTime(position)} / ${fmtTime(duration)}`;
}

// ------------------------------------------------------------------ toolbar

function hudWake() {
  $('#toolbar').classList.remove('hidden');
  document.body.classList.remove('hide-cursor');
  clearTimeout(hudTimer);
  if (!wall) return;
  hudTimer = setTimeout(() => {
    $('#toolbar').classList.add('hidden');
    document.body.classList.add('hide-cursor');
  }, 2000);
}

function backToLobby() {
  if (!wall) return;
  if (fsCell) exitFullscreen();
  clearTimeout(hudTimer);
  cells.forEach((cell) => {
    cell.video.pause();
    cell.video.removeAttribute('src');
    cell.video.load();
  });
  cells = [];
  wall = null;
  paused = false;
  $('#wall').hidden = true;
  $('#lobby').hidden = false;
  $('#toolbar').classList.remove('hidden');
  document.body.classList.remove('hide-cursor');
  refreshCounts(state.dirs);
}

async function quit() {
  try {
    await api('/api/quit', {});
  } catch {
    /* process exits */
  }
  window.close();
}

// --------------------------------------------------------------------- init

function bindEvents() {
  $('#addFolder').addEventListener('click', addFolders);
  $('#startBtn').addEventListener('click', () => startWall(false));
  $('#rows').addEventListener('input', syncGrid);
  $('#cols').addEventListener('input', syncGrid);
  $$('.preset').forEach((button) => {
    button.addEventListener('click', () => {
      $('#rows').value = button.dataset.rows;
      $('#cols').value = button.dataset.cols;
      syncGrid();
    });
  });

  $('#setup').addEventListener('click', backToLobby);
  $('#reshuffle').addEventListener('click', reshuffle);
  $('#pause').addEventListener('click', togglePause);
  $('#mute').addEventListener('click', toggleMute);

  $('#metaClose').addEventListener('click', closeMeta);
  $('#metaModal').addEventListener('click', (event) => {
    if (event.target.id === 'metaModal') closeMeta();
  });
  $('#metaCopy').addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(metaText);
      toast('Metadata copied to clipboard');
    } catch (err) {
      toast(`Copy failed: ${err.message}`);
    }
  });

  $('#errOk').addEventListener('click', () => { $('#errModal').hidden = true; });
  $('#help').addEventListener('click', () => { $('#help').hidden = true; });

  $('#fs').addEventListener('dblclick', (event) => {
    if (event.target.closest('.fs-controls')) return;
    exitFullscreen();
  });
  $('#fsSlider').addEventListener('input', updateFsUi);
  $('#fsSlider').addEventListener('pointerup', () => {
    if (!fsCell || !fsCell.entry) return;
    if (fsCell.usingFallback || fsCell.entry.mode !== 'direct') return;
    const video = fsCell.video;
    if (!Number.isFinite(video.duration)) return;
    video.currentTime = (Number($('#fsSlider').value) / 100) * video.duration;
  });

  document.addEventListener('mousemove', () => { if (wall) hudWake(); }, { passive: true });
  document.addEventListener('mousedown', () => { if (wall) hudWake(); }, { passive: true });
  document.addEventListener('wheel', () => { if (wall) hudWake(); }, { passive: true });

  window.addEventListener('resize', () => {
    cells.forEach(applyRotation);
    updateStatus();
  });

  document.addEventListener('keydown', (event) => {
    const target = event.target;
    if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
    if (!wall) return;
    hudWake();

    if (event.key === 'Escape') {
      if (!$('#help').hidden) { $('#help').hidden = true; return; }
      if (!$('#metaModal').hidden) { closeMeta(); return; }
      if (fsCell) { exitFullscreen(); return; }
      backToLobby();
      return;
    }
    if (event.key === 'Tab') {
      event.preventDefault();
      backToLobby();
      return;
    }
    if (event.key === 'r' || event.key === 'R') reshuffle();
    else if (event.key === 'p' || event.key === 'P') togglePause();
    else if (event.key === 'm' || event.key === 'M') toggleMute();
    else if (event.key === '?') $('#help').hidden = !$('#help').hidden;
    else if (event.key === 'q' || event.key === 'Q') quit();
  });
}

async function init() {
  bindEvents();
  const lobby = await api('/api/lobby');
  state.dirs = lobby.dirs || [];
  state.grid = lobby.grid || { rows: 2, cols: 2 };
  state.shuffle = !!lobby.shuffle;
  state.loop = !!lobby.loop;
  state.mute = !!lobby.mute;
  state.ffmpegAvailable = !!lobby.ffmpegAvailable;

  $('#rows').value = state.grid.rows;
  $('#cols').value = state.grid.cols;
  $('#shuffle').checked = state.shuffle;
  $('#loop').checked = state.loop;
  $('#mute').checked = state.mute;

  renderDirs();
  syncGrid();
  refreshCounts(state.dirs);
  if (lobby.autoStart) startWall(true);
}

init().catch((err) => showError(`Failed to start: ${err.message}`));
