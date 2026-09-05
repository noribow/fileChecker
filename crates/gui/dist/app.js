"use strict";

/* File Checker GUI frontend (§10.14). Vanilla JS, no build step — templates live in
 * index.html as <template> elements, cloned and populated per screen. All logic here
 * is presentation/navigation only; every check/scan/hash/DB operation goes through
 * `invoke()` into the Rust commands in crates/gui/src/commands (§10.13's "no core
 * logic in the GUI layer" applies to this layer too). */

const tauri = window.__TAURI__;
const core = tauri.core;
const dialog = tauri.dialog;

function invoke(cmd, args) {
  return core.invoke(cmd, args);
}

const PASSWORD_LOCKED_MARKER = "登録パスワード設定ファイルがロックされています";

/** Runs `invoke(cmd, args)`, and if it fails because the password store needs
 * unlocking first (§10.10), prompts for the master password and retries once. */
async function invokeWithPasswordRetry(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    const message = String(e);
    if (!message.includes(PASSWORD_LOCKED_MARKER)) throw e;
    await promptUnlockPasswordStore();
    return await invoke(cmd, args);
  }
}

// ---- toast -------------------------------------------------------------------------

function toast(message, isError) {
  const root = document.getElementById("toast-root");
  const el = document.createElement("div");
  el.className = "toast" + (isError ? " error" : "");
  el.textContent = message;
  root.appendChild(el);
  setTimeout(() => el.remove(), isError ? 6000 : 3200);
}

function errorMessage(e) {
  return typeof e === "string" ? e : e && e.message ? e.message : String(e);
}

// ---- formatting helpers --------------------------------------------------------------

function formatBytes(n) {
  if (n === null || n === undefined) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return (i === 0 ? v : v.toFixed(1)) + " " + units[i];
}

function formatDate(ms) {
  if (!ms) return "—";
  return new Date(ms).toLocaleString();
}

const STATUS_LABELS = {
  ok: "OK",
  corrupted: "破損",
  missing: "欠落",
  extra: "余剰",
  error: "エラー",
};

/** §10.14's "親アーカイブ名 > 内部相対パス" display. `scanned_file.path` for an
 * archive-nested entry is built as `{archive}/{entry}` (recursively), so a path
 * segment ending in .zip/.7z (case-insensitive) other than the final segment marks an
 * archive boundary — the same extension-based detection the backend itself uses
 * (`ArchiveFormat::detect`), so this never disagrees with how the scan actually
 * treated the file. Plain nested folders never end in .zip/.7z themselves without the
 * backend already treating them as an archive, so this doesn't misfire on those.
 */
function renderPath(path) {
  const segments = path.split("/");
  const isArchiveSegment = (s) => /\.(zip|7z)$/i.test(s);
  let html = "";
  let sawArchive = false;
  for (let i = 0; i < segments.length; i++) {
    const seg = segments[i];
    const isLast = i === segments.length - 1;
    if (!isLast && isArchiveSegment(seg)) {
      sawArchive = true;
      html += `<span class="archive-nested">📦 ${escapeHtml(seg)}</span> <span class="archive-nested">›</span> `;
    } else {
      html += escapeHtml(seg);
    }
  }
  return sawArchive ? html : escapeHtml(path);
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

// ---- dialogs -------------------------------------------------------------------------

async function pickFolder() {
  const result = await dialog.open({ directory: true, multiple: false });
  return Array.isArray(result) ? result[0] : result;
}

async function pickFile(filters) {
  const result = await dialog.open({ directory: false, multiple: false, filters });
  return Array.isArray(result) ? result[0] : result;
}

async function pickSaveFile(defaultName, filters) {
  return await dialog.save({ defaultPath: defaultName, filters });
}

// ---- generic modal -------------------------------------------------------------------

function openModal(templateId, setup) {
  return new Promise((resolve) => {
    const tpl = document.getElementById(templateId);
    const node = tpl.content.firstElementChild.cloneNode(true);
    document.getElementById("modal-root").appendChild(node);

    let settled = false;
    const close = (value) => {
      if (settled) return;
      settled = true;
      node.remove();
      resolve(value);
    };

    node.addEventListener("click", (ev) => {
      if (ev.target === node) close(null);
    });
    node.querySelectorAll("[data-action='cancel']").forEach((b) => b.addEventListener("click", () => close(null)));

    setup(node, close);
  });
}

function showError(node, message) {
  const el = node.querySelector("[data-field='error']");
  if (!el) return;
  el.textContent = message;
  el.hidden = false;
}

async function confirmModal(title, message) {
  return openModal("tpl-modal-confirm", (node, close) => {
    node.querySelector("[data-field='title']").textContent = title;
    node.querySelector("[data-field='message']").textContent = message;
    node.querySelector("[data-action='submit']").addEventListener("click", () => close(true));
  }).then((v) => v === true);
}

function masterPasswordModal(opts) {
  // opts: { title, description, needsCurrent, warning, onSubmit(values) -> Promise }
  return openModal("tpl-modal-master-password", (node, close) => {
    node.querySelector("[data-field='title']").textContent = opts.title;
    node.querySelector("[data-field='description']").textContent = opts.description || "";
    if (opts.warning) {
      const w = node.querySelector("[data-field='warning']");
      w.textContent = opts.warning;
      w.hidden = false;
    }
    if (opts.needsCurrent) {
      node.querySelector("[data-field='current-row']").hidden = false;
    }
    node.querySelector("[data-action='submit']").addEventListener("click", async () => {
      const current = node.querySelector("[data-field='current']").value;
      const password = node.querySelector("[data-field='password']").value;
      try {
        await opts.onSubmit({ current, password });
        close(true);
      } catch (e) {
        showError(node, errorMessage(e));
      }
    });
  });
}

async function promptCreatePasswordStore() {
  await masterPasswordModal({
    title: "マスターパスワードの初回設定",
    description: "登録パスワード設定ファイルを保護するマスターパスワードを設定します。",
    onSubmit: async ({ password }) => {
      if (!password) throw new Error("マスターパスワードを入力してください");
      await invoke("password_store_create", { masterPassword: password });
    },
  });
}

async function promptUnlockPasswordStore() {
  await masterPasswordModal({
    title: "マスターパスワードの入力",
    description: "パスワード保護アーカイブの復号に登録パスワードを使用するため、マスターパスワードを入力してください。",
    onSubmit: async ({ password }) => {
      await invoke("password_store_unlock", { masterPassword: password });
    },
  });
}

async function promptChangeMasterPassword() {
  return openModal("tpl-modal-master-password", (node, close) => {
    node.querySelector("[data-field='title']").textContent = "マスターパスワードの変更";
    node.querySelector("[data-field='current-row']").hidden = false;
    node.querySelector("[data-field='password-label']").textContent = "新しいマスターパスワード";
    node.querySelector("[data-action='submit']").addEventListener("click", async () => {
      const current = node.querySelector("[data-field='current']").value;
      const next = node.querySelector("[data-field='password']").value;
      try {
        await invoke("master_password_change", { currentMasterPassword: current, newMasterPassword: next });
        toast("マスターパスワードを変更しました");
        close(true);
      } catch (e) {
        showError(node, errorMessage(e));
      }
    });
  });
}

async function promptMediaLabel() {
  return openModal("tpl-modal-media-label", (node, close) => {
    node.querySelector("[data-action='submit']").addEventListener("click", () => {
      const label = node.querySelector("[data-field='label']").value.trim();
      if (!label) {
        showError(node, "ラベルを入力してください");
        return;
      }
      close(label);
    });
  });
}

// ---- navigation ------------------------------------------------------------------------

const main = document.getElementById("main");
let currentScreen = null;

function setActiveNav(name) {
  document.querySelectorAll(".nav-btn").forEach((b) => b.classList.toggle("active", b.dataset.nav === name));
}

const SCREEN_LOADERS = {};

async function nav(screen, params) {
  currentScreen = screen;
  const tpl = document.getElementById("tpl-" + screen);
  main.innerHTML = "";
  const node = tpl.content.firstElementChild.cloneNode(true);
  main.appendChild(node);
  setActiveNav(navGroupFor(screen));
  const loader = SCREEN_LOADERS[screen];
  if (loader) {
    try {
      await loader(node, params || {});
    } catch (e) {
      toast(errorMessage(e), true);
    }
  }
}

function navGroupFor(screen) {
  if (screen === "home") return "home";
  if (screen.startsWith("reference") || screen.startsWith("integrity") || screen.startsWith("reconstruct")) return "reference-list";
  if (screen.startsWith("duplicate") || screen === "media-manage") return "duplicate-targets";
  if (screen === "history") return "history";
  if (screen.startsWith("settings")) return "settings-general";
  return "";
}

document.querySelectorAll(".nav-btn").forEach((btn) => {
  btn.addEventListener("click", () => nav(btn.dataset.nav));
});

// ================= HOME =================

SCREEN_LOADERS["home"] = async (node) => {
  const summary = await invoke("home_summary");
  node.querySelector("[data-field='reference_set_count']").textContent = summary.reference_set_count;
  node.querySelector("[data-field='removable_media_count']").textContent = summary.removable_media_count;
  const tbody = node.querySelector("[data-field='recent-runs']");
  tbody.innerHTML = "";
  for (const run of summary.recent_check_runs) {
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td>${run.id}</td>
      <td>${run.check_type === "integrity" ? "整合性" : "重複"}</td>
      <td>${formatDate(run.started_at)}</td>
      <td>${run.status}</td>
      <td>${escapeHtml(run.summary_text)}</td>
      <td><button data-open="${run.id}" data-type="${run.check_type}">開く</button></td>`;
    tbody.appendChild(tr);
  }
  tbody.querySelectorAll("button[data-open]").forEach((b) => {
    b.addEventListener("click", () => openCheckRun(Number(b.dataset.open), b.dataset.type));
  });

  node.querySelector("[data-action='go-integrity']").addEventListener("click", () => nav("reference-list"));
  node.querySelector("[data-action='go-duplicate']").addEventListener("click", () => nav("duplicate-targets"));
};

async function openCheckRun(id, type) {
  if (type === "integrity") {
    await nav("integrity-result", { checkRunId: id, titleSuffix: `(#${id})` });
  } else {
    await nav("duplicate-result", { checkRunId: id });
  }
}

// ================= REFERENCE LIST / NEW =================

SCREEN_LOADERS["reference-list"] = async (node) => {
  const sets = await invoke("reference_list");
  const byName = new Map();
  for (const s of sets) {
    if (!byName.has(s.name)) byName.set(s.name, []);
    byName.get(s.name).push(s);
  }
  const tbody = node.querySelector("[data-field='reference-sets']");
  tbody.innerHTML = "";
  for (const [name, versions] of byName) {
    versions.sort((a, b) => b.version - a.version);
    versions.forEach((s, idx) => {
      const tr = document.createElement("tr");
      if (idx > 0) tr.className = "muted";
      tr.innerHTML = `
        <td>${idx === 0 ? escapeHtml(name) : ""}</td>
        <td>v${s.version}</td>
        <td>${escapeHtml(s.source_format)}</td>
        <td>${formatDate(s.created_at)}</td>
        <td>
          <button data-run="${s.id}">このセットで実行</button>
          ${idx === 0 ? `<button data-resupersede="${s.id}" data-name="${escapeHtml(name)}">再スキャンして新バージョン作成</button>` : ""}
        </td>`;
      tbody.appendChild(tr);
    });
  }
  tbody.querySelectorAll("button[data-run]").forEach((b) => {
    const set = sets.find((s) => s.id === Number(b.dataset.run));
    b.addEventListener("click", () => nav("integrity-run", { referenceSetId: set.id, referenceSetName: set.name }));
  });
  tbody.querySelectorAll("button[data-resupersede]").forEach((b) => {
    b.addEventListener("click", () =>
      nav("reference-new", { supersede: Number(b.dataset.resupersede), fixedName: b.dataset.name })
    );
  });

  node.querySelector("[data-action='new-reference']").addEventListener("click", () => nav("reference-new", {}));
};

SCREEN_LOADERS["reference-new"] = async (node, params) => {
  let scanFolderPath = null;
  let importFilePath = null;

  if (params.supersede) {
    node.querySelector("[data-field='ref-name']").value = params.fixedName;
    node.querySelector("[data-field='ref-name']").disabled = true;
    node.querySelector("[data-field='supersede-hint']").hidden = false;
  }

  node.querySelectorAll(".tab-btn").forEach((tab) => {
    tab.addEventListener("click", () => {
      node.querySelectorAll(".tab-btn").forEach((t) => t.classList.remove("active"));
      tab.classList.add("active");
      node.querySelectorAll("[data-tabpanel]").forEach((p) => (p.hidden = p.dataset.tabpanel !== tab.dataset.tab));
    });
  });

  node.querySelector("[data-action='pick-scan-folder']").addEventListener("click", async () => {
    const p = await pickFolder();
    if (!p) return;
    scanFolderPath = p;
    node.querySelector("[data-field='scan-folder-path']").textContent = p;
  });

  node.querySelector("[data-action='run-generate-from-folder']").addEventListener("click", async () => {
    const name = node.querySelector("[data-field='ref-name']").value.trim();
    if (!scanFolderPath) return toast("対象フォルダを選択してください", true);
    if (!name) return toast("セット名を入力してください", true);
    try {
      const summary = await invokeWithPasswordRetry("reference_generate_from_folder", {
        folderPath: scanFolderPath,
        name,
        supersede: params.supersede || null,
      });
      toast(`お手本セットを作成しました（${summary.file_count}件、エラー${summary.error_count}件）`);
      nav("reference-list");
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });

  const formatSelect = node.querySelector("[data-field='import-format']");
  const mergeModeRow = node.querySelector("[data-field='merge-mode-row']");
  const syncMergeModeVisibility = () => (mergeModeRow.hidden = formatSelect.value !== "mame-machinelist");
  formatSelect.addEventListener("change", syncMergeModeVisibility);
  syncMergeModeVisibility();

  node.querySelector("[data-action='pick-import-file']").addEventListener("click", async () => {
    const p = await pickFile([{ name: "定義ファイル", extensions: ["xml", "dat", "txt"] }]);
    if (!p) return;
    importFilePath = p;
    node.querySelector("[data-field='import-file-path']").textContent = p;
  });

  node.querySelector("[data-action='run-import']").addEventListener("click", async () => {
    const name = node.querySelector("[data-field='import-name']").value.trim();
    if (!importFilePath) return toast("取り込みファイルを選択してください", true);
    if (!name) return toast("セット名を入力してください", true);
    try {
      const summary = await invoke("reference_import_mame", {
        filePath: importFilePath,
        format: formatSelect.value,
        name,
        mergeMode: formatSelect.value === "mame-machinelist" ? node.querySelector("[data-field='import-merge-mode']").value : null,
        includeBaddump: node.querySelector("[data-field='import-include-baddump']").checked,
      });
      toast(`取り込みました（${summary.imported_count}件、除外${summary.excluded_count}件）`);
      nav("reference-list");
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });
};

// ================= INTEGRITY RUN / RESULT =================

SCREEN_LOADERS["integrity-run"] = async (node, params) => {
  let referenceSetId = params.referenceSetId || null;
  let targetFolderPath = null;
  const nameField = node.querySelector("[data-field='reference-set-name']");

  if (referenceSetId) {
    nameField.textContent = `${params.referenceSetName} (#${referenceSetId})`;
  } else {
    const sets = await invoke("reference_list");
    const select = document.createElement("select");
    select.innerHTML = sets.map((s) => `<option value="${s.id}">${escapeHtml(s.name)} (v${s.version})</option>`).join("");
    select.addEventListener("change", () => (referenceSetId = Number(select.value)));
    referenceSetId = sets.length ? sets[0].id : null;
    nameField.replaceWith(select);
  }

  const scanRunSelect = node.querySelector("[data-field='target-scan-run']");
  const history = await invoke("scan_history_list", { limit: 100 });
  for (const run of history.filter((r) => r.target_type === "folder")) {
    const opt = document.createElement("option");
    opt.value = run.id;
    opt.textContent = `#${run.id} ${run.folder_path} (${formatDate(run.started_at)}, ${run.file_count}件)`;
    scanRunSelect.appendChild(opt);
  }
  if (params.preselectScanRunId) scanRunSelect.value = String(params.preselectScanRunId);

  node.querySelector("[data-action='pick-target-folder']").addEventListener("click", async () => {
    const p = await pickFolder();
    if (!p) return;
    targetFolderPath = p;
    scanRunSelect.value = "";
    node.querySelector("[data-field='target-folder-path']").textContent = p;
  });
  scanRunSelect.addEventListener("change", () => {
    if (scanRunSelect.value) {
      targetFolderPath = null;
      node.querySelector("[data-field='target-folder-path']").textContent = "(未選択)";
    }
  });

  node.querySelector("[data-action='run-integrity']").addEventListener("click", async () => {
    if (!referenceSetId) return toast("お手本セットを選択してください", true);
    const scanRunIds = scanRunSelect.value ? [Number(scanRunSelect.value)] : [];
    if (!targetFolderPath && scanRunIds.length === 0) return toast("対象フォルダまたはスキャン履歴を選択してください", true);
    try {
      const result = await invokeWithPasswordRetry("integrity_run", {
        referenceSetId,
        folderPath: targetFolderPath,
        scanRunIds,
      });
      toast("整合性チェックが完了しました");
      await nav("integrity-result", {
        checkRunId: result.check_run_id,
        titleSuffix: `— ${result.reference_set_name} v${result.reference_set_version}`,
      });
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });
};

const INTEGRITY_STATUSES = ["ok", "corrupted", "missing", "extra", "error"];

SCREEN_LOADERS["integrity-result"] = async (node, params) => {
  const checkRunId = params.checkRunId;
  node.querySelector("[data-field='title-suffix']").textContent = params.titleSuffix || `(#${checkRunId})`;

  const counts = await invoke("integrity_counts", { checkRunId });
  let activeFilter = new Set(["corrupted", "missing", "extra", "error"]);

  const badgesEl = node.querySelector("[data-field='badges']");
  const renderBadges = () => {
    badgesEl.innerHTML = "";
    for (const status of INTEGRITY_STATUSES) {
      const b = document.createElement("button");
      b.className = "badge" + (activeFilter.has(status) ? " active" : "") + (status === "ok" ? " ok" : status === "error" || status === "corrupted" ? " danger" : " warn");
      b.textContent = `${STATUS_LABELS[status]}: ${counts[status]}`;
      b.addEventListener("click", async () => {
        if (activeFilter.has(status)) activeFilter.delete(status);
        else activeFilter.add(status);
        renderBadges();
        await loadRows();
      });
      badgesEl.appendChild(b);
    }
  };
  renderBadges();

  const tbody = node.querySelector("[data-field='rows']");
  let currentRows = [];
  const loadRows = async () => {
    currentRows = await invoke("integrity_results", { checkRunId, statusFilter: Array.from(activeFilter) });
    renderRows();
  };
  const renderRows = () => {
    const search = node.querySelector("[data-field='search']").value.trim().toLowerCase();
    tbody.innerHTML = "";
    for (const r of currentRows) {
      if (search && !r.path.toLowerCase().includes(search)) continue;
      const tr = document.createElement("tr");
      tr.innerHTML = `
        <td>${renderPath(r.path)}</td>
        <td>${formatBytes(r.size)}</td>
        <td><span class="status-pill status-${r.result_status}">${STATUS_LABELS[r.result_status]}</span></td>
        <td class="muted">${escapeHtml(r.detail || "")}</td>`;
      tbody.appendChild(tr);
    }
  };
  node.querySelector("[data-field='search']").addEventListener("input", renderRows);
  await loadRows();

  node.querySelector("[data-action='export-csv']").addEventListener("click", () => exportCheck(checkRunId, "csv"));
  node.querySelector("[data-action='export-json']").addEventListener("click", () => exportCheck(checkRunId, "json"));
  node.querySelector("[data-action='go-reconstruct']").addEventListener("click", () => nav("reconstruct-plan", { checkRunId }));
};

async function exportCheck(checkRunId, format) {
  const path = await pickSaveFile(`check_run_${checkRunId}.${format}`, [{ name: format.toUpperCase(), extensions: [format] }]);
  if (!path) return;
  try {
    await invoke("report_export", { checkRunId, format, outputPath: path });
    toast(`書き出しました: ${path}`);
  } catch (e) {
    toast(errorMessage(e), true);
  }
}

// ================= DUPLICATE TARGETS / RESULT =================

SCREEN_LOADERS["duplicate-targets"] = async (node, params) => {
  const folders = [];
  const scanRunIds = new Set(params.preselectScanRunId ? [params.preselectScanRunId] : []);

  const folderList = node.querySelector("[data-field='folder-list']");
  const renderFolders = () => {
    folderList.innerHTML = "";
    folders.forEach((f, i) => {
      const li = document.createElement("li");
      li.innerHTML = `<span class="mono">${escapeHtml(f)}</span> <button data-remove="${i}">削除</button>`;
      folderList.appendChild(li);
    });
    folderList.querySelectorAll("button[data-remove]").forEach((b) =>
      b.addEventListener("click", () => {
        folders.splice(Number(b.dataset.remove), 1);
        renderFolders();
      })
    );
  };
  renderFolders();

  node.querySelector("[data-action='add-target-folder']").addEventListener("click", async () => {
    const p = await pickFolder();
    if (!p) return;
    folders.push(p);
    renderFolders();
  });

  const mediaListEl = node.querySelector("[data-field='media-list']");
  const renderConnectedMedia = async () => {
    const [connected, known] = await Promise.all([invoke("media_connected"), invoke("media_list")]);
    mediaListEl.innerHTML = "";
    for (const d of connected) {
      const match = known.find((m) => m.identifier_type === d.identifier_type && m.identifier_value === d.identifier_value);
      const li = document.createElement("li");
      const label = d.display_name || d.mount_path;
      li.innerHTML = `<span>${escapeHtml(label)} <span class="muted mono">${escapeHtml(d.mount_path)}</span></span>
        <span>
          ${match ? `<button data-reuse="${match.id}">保存済みスキャン結果を使用</button>` : ""}
          <button data-scan-mount="${escapeHtml(d.mount_path)}">${match ? "再スキャン" : "今すぐスキャン"}</button>
        </span>`;
      mediaListEl.appendChild(li);
    }
    mediaListEl.querySelectorAll("button[data-reuse]").forEach((b) =>
      b.addEventListener("click", () => {
        scanRunIdsAddLatestForMedia(Number(b.dataset.reuse));
      })
    );
    mediaListEl.querySelectorAll("button[data-scan-mount]").forEach((b) =>
      b.addEventListener("click", async () => {
        try {
          const summary = await invokeWithPasswordRetry("media_scan_by_mount", {
            mountPath: b.dataset.scanMount,
            manualLabel: null,
          }).catch(async (e) => {
            if (String(e).includes("ラベルを入力してください")) {
              const label = await promptMediaLabel();
              if (!label) return null;
              return invokeWithPasswordRetry("media_scan_by_mount", { mountPath: b.dataset.scanMount, manualLabel: label });
            }
            throw e;
          });
          if (!summary) return;
          scanRunIds.add(summary.scan_run_id);
          renderHistorySelection();
          toast("メディアをスキャンしました");
        } catch (e) {
          toast(errorMessage(e), true);
        }
      })
    );
  };

  const historyListEl = node.querySelector("[data-field='history-list']");
  let historyRows = [];
  const renderHistorySelection = () => {
    historyListEl.innerHTML = "";
    for (const r of historyRows) {
      const li = document.createElement("li");
      const target = r.target_type === "folder" ? r.folder_path : r.removable_media_display_name || `メディア#${r.removable_media_id}`;
      li.innerHTML = `<label><input type="checkbox" data-history="${r.id}" ${scanRunIds.has(r.id) ? "checked" : ""}/>
        #${r.id} ${escapeHtml(target || "")} (${formatDate(r.started_at)}, ${r.file_count}件)</label>`;
      historyListEl.appendChild(li);
    }
    historyListEl.querySelectorAll("input[data-history]").forEach((cb) =>
      cb.addEventListener("change", () => {
        const id = Number(cb.dataset.history);
        if (cb.checked) scanRunIds.add(id);
        else scanRunIds.delete(id);
      })
    );
  };
  const loadHistory = async () => {
    historyRows = await invoke("scan_history_list", { limit: 50 });
    renderHistorySelection();
  };
  const scanRunIdsAddLatestForMedia = (mediaId) => {
    const latest = historyRows.find((r) => r.removable_media_id === mediaId);
    if (latest) {
      scanRunIds.add(latest.id);
      renderHistorySelection();
    } else {
      toast("このメディアの保存済みスキャン結果が見つかりません", true);
    }
  };

  node.querySelector("[data-action='refresh-connected-media']").addEventListener("click", renderConnectedMedia);
  node.querySelector("[data-action='go-media-manage']").addEventListener("click", () => nav("media-manage"));

  await Promise.all([renderConnectedMedia(), loadHistory()]);

  node.querySelector("[data-action='run-duplicate']").addEventListener("click", async () => {
    if (folders.length === 0 && scanRunIds.size === 0) return toast("対象フォルダまたはメディア・履歴を選択してください", true);
    try {
      const result = await invokeWithPasswordRetry("duplicate_run", {
        folderPaths: folders,
        scanRunIds: Array.from(scanRunIds),
      });
      toast("重複チェックが完了しました");
      await nav("duplicate-result", { checkRunId: result.check_run_id, summary: result });
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });
};

SCREEN_LOADERS["media-manage"] = async (node) => {
  const [known, connected] = await Promise.all([invoke("media_list"), invoke("media_connected")]);
  const tbody = node.querySelector("[data-field='media-rows']");
  tbody.innerHTML = "";
  for (const m of known) {
    const isConnected = connected.some((d) => d.identifier_type === m.identifier_type && d.identifier_value === m.identifier_value);
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td>${escapeHtml(m.display_name || "(no name)")}</td>
      <td class="mono">${escapeHtml(m.identifier_type)}=${escapeHtml(m.identifier_value)}</td>
      <td>${formatDate(m.last_seen_at)}</td>
      <td><button data-scan="${m.id}" ${isConnected ? "" : "disabled"}>今すぐスキャン</button></td>`;
    tbody.appendChild(tr);
  }
  tbody.querySelectorAll("button[data-scan]").forEach((b) =>
    b.addEventListener("click", async () => {
      try {
        await invokeWithPasswordRetry("media_scan_by_id", { mediaId: Number(b.dataset.scan) });
        toast("スキャンしました");
      } catch (e) {
        toast(errorMessage(e), true);
      }
    })
  );
};

SCREEN_LOADERS["duplicate-result"] = async (node, params) => {
  const checkRunId = params.checkRunId;
  const summary = params.summary;
  if (summary) {
    node.querySelector("[data-field='group-count']").textContent = summary.group_count;
    node.querySelector("[data-field='file-count']").textContent = summary.duplicate_file_count;
    node.querySelector("[data-field='reclaimable']").textContent = formatBytes(summary.reclaimable_bytes);
    node.querySelector("[data-field='error-count']").textContent = summary.error_count;
  }

  const groups = await invoke("duplicate_groups", { checkRunId });
  if (!summary) {
    node.querySelector("[data-field='group-count']").textContent = groups.length;
    node.querySelector("[data-field='file-count']").textContent = groups.reduce((a, g) => a + g.member_count, 0);
    node.querySelector("[data-field='reclaimable']").textContent = formatBytes(
      groups.reduce((a, g) => a + g.size * Math.max(0, g.member_count - 1), 0)
    );
    node.querySelector("[data-field='error-count']").textContent = "—";
  }

  const container = node.querySelector("[data-field='groups']");
  container.innerHTML = "";
  for (const g of groups) {
    const details = document.createElement("details");
    details.innerHTML = `<summary>${formatBytes(g.size)} × ${g.member_count}件 <span class="mono muted">${g.sha256_hex.slice(0, 12)}…</span></summary>`;
    const ul = document.createElement("ul");
    ul.className = "list-plain";
    for (const m of g.members) {
      const li = document.createElement("li");
      li.innerHTML = `<span class="mono">${escapeHtml(m.path)}</span><span class="muted">scan_run #${m.scan_run_id}</span>`;
      ul.appendChild(li);
    }
    details.appendChild(ul);
    container.appendChild(details);
  }

  node.querySelector("[data-action='export-dup-csv']").addEventListener("click", () => exportCheck(checkRunId, "csv"));
  node.querySelector("[data-action='export-dup-json']").addEventListener("click", () => exportCheck(checkRunId, "json"));
  node.querySelector("[data-action='rerun-duplicate']").addEventListener("click", () => nav("duplicate-targets"));
};

// ================= HISTORY =================

SCREEN_LOADERS["history"] = async (node) => {
  const rows = await invoke("scan_history_list", { limit: 200 });
  const tbody = node.querySelector("[data-field='rows']");
  tbody.innerHTML = "";
  for (const r of rows) {
    const target = r.target_type === "folder" ? r.folder_path : r.removable_media_display_name || `メディア#${r.removable_media_id}`;
    const tr = document.createElement("tr");
    tr.innerHTML = `
      <td>${r.id}</td>
      <td>${r.target_type === "folder" ? "フォルダ" : "リムーバブルメディア"}</td>
      <td class="mono">${escapeHtml(target || "")}</td>
      <td>${formatDate(r.started_at)}</td>
      <td>${r.status}</td>
      <td>${r.file_count}</td>`;
    tbody.appendChild(tr);
  }
};

// ================= SETTINGS =================

SCREEN_LOADERS["settings-general"] = async (node) => {
  node.querySelectorAll("[data-settings-tab]").forEach((tab) => {
    tab.addEventListener("click", () => {
      node.querySelectorAll("[data-settings-tab]").forEach((t) => t.classList.remove("active"));
      tab.classList.add("active");
      node.querySelectorAll("[data-settings-panel]").forEach((p) => (p.hidden = p.dataset.settingsPanel !== tab.dataset.settingsTab));
      if (tab.dataset.settingsTab === "passwords") loadPasswordsPanel(node);
    });
  });

  const general = await invoke("settings_get_general");
  node.querySelector("[data-field='archive-max-depth']").value = general.archive_max_depth;
  node.querySelector("[data-field='archive-size-limit']").value = general.archive_entry_size_limit_bytes;
  node.querySelector("[data-field='password-mode']").value = general.archive_password_mode;

  node.querySelector("[data-action='save-general']").addEventListener("click", async () => {
    try {
      await invoke("settings_set_general", {
        archiveMaxDepth: Number(node.querySelector("[data-field='archive-max-depth']").value),
        archiveEntrySizeLimitBytes: Number(node.querySelector("[data-field='archive-size-limit']").value),
        archivePasswordMode: node.querySelector("[data-field='password-mode']").value,
      });
      toast("設定を保存しました");
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });

  node.querySelector("[data-action='pw-unlock-prompt']").addEventListener("click", async () => {
    await promptUnlockPasswordStore();
    loadPasswordsPanel(node);
  });
  node.querySelector("[data-action='pw-lock']").addEventListener("click", async () => {
    await invoke("password_store_lock");
    loadPasswordsPanel(node);
  });
  node.querySelector("[data-action='pw-change-prompt']").addEventListener("click", async () => {
    await promptChangeMasterPassword();
    loadPasswordsPanel(node);
  });
  node.querySelector("[data-action='pw-reset-prompt']").addEventListener("click", async () => {
    const ok = await confirmModal(
      "登録パスワード設定ファイルのリセット",
      "登録済みのすべてのパスワードが失われます。この操作は取り消せません。よろしいですか？"
    );
    if (!ok) return;
    await invoke("master_password_reset");
    toast("リセットしました");
    loadPasswordsPanel(node);
  });
  node.querySelector("[data-action='pw-add']").addEventListener("click", async () => {
    const format = node.querySelector("[data-field='new-pw-format']").value || null;
    const password = node.querySelector("[data-field='new-pw-value']").value;
    if (!password) return toast("パスワードを入力してください", true);
    try {
      await invoke("password_add", { format, password });
      node.querySelector("[data-field='new-pw-value']").value = "";
      loadPasswordsPanel(node);
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });
};

async function loadPasswordsPanel(node) {
  const status = await invoke("password_store_status");
  const statusEl = node.querySelector("[data-field='lock-status']");
  const rows = node.querySelector("[data-field='password-rows']");
  const actionsRow = node.querySelector("[data-field='pw-actions']");
  const listTitle = node.querySelector("[data-field='pw-list-title']");
  const table = node.querySelector("[data-field='pw-table']");
  const addRow = node.querySelector("[data-field='pw-add-row']");

  if (!status.exists) {
    actionsRow.hidden = true;
    listTitle.hidden = true;
    table.hidden = true;
    addRow.hidden = true;
    statusEl.innerHTML = `<span class="muted">登録パスワード設定ファイルは未作成です。</span> <button data-action="pw-create-inline">初回設定…</button>`;
    statusEl.querySelector("[data-action='pw-create-inline']").addEventListener("click", async () => {
      await promptCreatePasswordStore();
      loadPasswordsPanel(node);
    });
    rows.innerHTML = "";
    return;
  }

  actionsRow.hidden = false;
  statusEl.textContent = status.unlocked ? "ロック解除済み" : "ロック中";
  if (!status.unlocked) {
    listTitle.hidden = true;
    table.hidden = true;
    addRow.hidden = true;
    rows.innerHTML = "";
    return;
  }
  listTitle.hidden = false;
  table.hidden = false;
  addRow.hidden = false;
  const list = await invoke("password_list");
  rows.innerHTML = "";
  for (const p of list) {
    const tr = document.createElement("tr");
    tr.innerHTML = `<td>${p.format || "(全形式共通)"}</td><td class="mono">${"•".repeat(Math.min(p.password.length, 16))}</td>
      <td><button data-remove-pw="${p.id}">削除</button></td>`;
    rows.appendChild(tr);
  }
  rows.querySelectorAll("button[data-remove-pw]").forEach((b) =>
    b.addEventListener("click", async () => {
      await invoke("password_remove", { id: b.dataset.removePw });
      loadPasswordsPanel(node);
    })
  );
}

// ================= RECONSTRUCT =================

SCREEN_LOADERS["reconstruct-plan"] = async (node, params) => {
  const checkRunId = params.checkRunId;
  let destinationPath = null;

  node.querySelector("[data-action='pick-destination-folder']").addEventListener("click", async () => {
    const p = await pickFolder();
    if (!p) return;
    destinationPath = p;
    node.querySelector("[data-field='destination-path']").textContent = p;
  });

  node.querySelector("[data-action='compute-plan']").addEventListener("click", async () => {
    if (!destinationPath) return toast("再構成先フォルダを選択してください", true);
    try {
      const plan = await invokeWithPasswordRetry("reconstruct_plan", {
        checkRunId,
        destination: { path: destinationPath },
      });
      node.querySelector("[data-field='plan-summary']").hidden = false;
      node.querySelector("[data-field='resolved-count']").textContent = plan.resolved.length;
      node.querySelector("[data-field='missing-count']").textContent = plan.missing.length;
      const ul = node.querySelector("[data-field='missing-list']");
      ul.innerHTML = "";
      for (const m of plan.missing) {
        const li = document.createElement("li");
        li.innerHTML = `<span class="mono">${escapeHtml(m.path)}</span>`;
        ul.appendChild(li);
      }
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });

  node.querySelector("[data-action='start-reconstruct']").addEventListener("click", async () => {
    try {
      const reconstructionRunId = await invokeWithPasswordRetry("reconstruct_start", {
        checkRunId,
        destination: { path: destinationPath },
      });
      await nav("reconstruct-progress", { reconstructionRunId });
    } catch (e) {
      toast(errorMessage(e), true);
    }
  });
};

SCREEN_LOADERS["reconstruct-progress"] = async (node, params) => {
  const reconstructionRunId = params.reconstructionRunId;

  const refresh = async () => {
    const status = await invoke("reconstruct_status", { reconstructionRunId });
    node.querySelector("[data-field='written']").textContent = status.written;
    node.querySelector("[data-field='error']").textContent = status.error;
    node.querySelector("[data-field='pending']").textContent = status.pending;
  };

  const runPass = async () => {
    const result = await invokeWithPasswordRetry("reconstruct_run_pass", { reconstructionRunId });
    await refresh();
    const ul = node.querySelector("[data-field='waiting-media']");
    ul.innerHTML = "";
    for (const m of result.still_needed_removable_media) {
      const li = document.createElement("li");
      li.textContent = m.label;
      ul.appendChild(li);
    }
    if (result.still_needed_removable_media.length === 0) {
      const status = await invoke("reconstruct_status", { reconstructionRunId });
      if (status.pending === 0) {
        toast(status.error > 0 ? `再構成が完了しました（エラー ${status.error}件）` : "再構成が完了しました");
      }
    }
  };

  node.querySelector("[data-action='retry-pass']").addEventListener("click", () => runPass().catch((e) => toast(errorMessage(e), true)));

  await refresh();
  await runPass();
};

// ---- boot --------------------------------------------------------------------------

nav("home");
